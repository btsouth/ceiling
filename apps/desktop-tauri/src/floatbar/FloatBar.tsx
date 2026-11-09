import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { useProviders } from "../hooks/useProviders";
import {
  getForegroundProvider,
  getSettingsSnapshot,
  openSettingsWindow,
  refreshProvidersIfStale,
} from "../lib/tauri";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import { getProviderIcon } from "../components/providers/providerIcons";
import type {
  BootstrapState,
  CapacityEventPayload,
  ForegroundProviderSnapshot,
  FloatBarSelectionMode,
  ProviderUsageSnapshot,
  SettingsSnapshot,
} from "../types/bridge";
import {
  FLOAT_BAR_CONFIG_CHANGED_EVENT,
  hideFloatBar,
  resizeFloatBar,
  setFloatBarClickThrough,
} from "./api";
import FloatBarMenu from "./FloatBarMenu";
import { selectVisibleFloatBarProviders } from "./selection";
import {
  capacityFreshness,
  constrainingWindow,
  activePromoBoosts,
  calmPresentation,
  stripAmountLabel,
  type CapacityFreshness,
  type ConstrainingWindow,
} from "../lib/capacityPresentation";
import { compareAccountIds } from "../lib/providerRow";
import type { FloatBarInformationMode } from "../types/bridge";
import "./FloatBar.css";

function ResetIcon({ size }: { size: number }) {
  return (
    <svg
      className="floatbar__reset-icon-svg"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <path
        d="M12.9 7.1a5 5 0 1 0-1.2 3.9"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
      <path
        d="M12.9 3.8v3.3H9.6"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function inlineResetTime(resetText: string): string {
  const normalized = resetText.trim();
  if (/^reset(?:s|ting)?(?:\s+due)?\s*(?:now)?$/i.test(normalized)) {
    return "now";
  }
  return normalized
    .replace(/^resets?\s+in\s+/i, "")
    .replace(/^resets?\s+/i, "")
    .trim();
}

function providerKey(provider: ProviderUsageSnapshot): string {
  return `${provider.providerId}:${provider.accountEmail ?? ""}`;
}

/**
 * Whether a reading belongs on the always-visible capacity strip.
 *
 * Enabling a provider in Settings does not mean it has capacity yet. Until the
 * first successful fetch, the cache holds an error placeholder (CLI missing,
 * language server not running, sign-in needed). Those belong on Overview and
 * Settings so the user can finish setup — not as a "—" pill that steals space
 * and, for providers whose session window is named after a model family
 * (Antigravity's primary label is "Claude"), mislabels the seat entirely.
 *
 * Matches widget-snapshot / tray policy: only error-free readings are
 * capacity-ready.
 */
export function isFloatBarEligible(
  provider: Pick<ProviderUsageSnapshot, "error">,
): boolean {
  const error = provider.error?.trim();
  return error == null || error.length === 0;
}

/**
 * One-number strip meter. Cursor uses actionable Auto/API selection inside
 * `constrainingWindow` (a maxed API bar must not hide Auto that still has room).
 * Overview still shows Plan as the hero via `glanceMeters`.
 */
function floatBarWindow(provider: ProviderUsageSnapshot): ConstrainingWindow {
  return constrainingWindow(provider);
}

/** Named-state placeholders are not heat (SBS-876). */
function floatBarHeat(provider: ProviderUsageSnapshot): number {
  const hero = floatBarWindow(provider);
  return hero.namedState ? -1 : hero.window.usedPercent;
}

/**
 * The capacity pill shown for a single provider.
 *
 * Hero % is the primary plan pool (same as overview cards). A hot companion
 * lane (Auto/API/5-hour) appears as a quiet secondary chip. Tone still
 * follows the constraining window so pressure isn't hidden.
 */
function ProviderPill({
  provider,
  highRemaining,
  critRemaining,
  showAsUsed,
  scale,
  showResetInline,
  resetRelative,
  usedSuffix,
  remainingSuffix,
  capacityEventKind,
  informationMode,
  isCompact,
  expanded,
  onToggleExpand,
}: {
  provider: ProviderUsageSnapshot;
  highRemaining: number;
  critRemaining: number;
  showAsUsed: boolean;
  scale: number;
  showResetInline: boolean;
  resetRelative: boolean;
  usedSuffix: string;
  remainingSuffix: string;
  capacityEventKind?: CapacityEventPayload["kind"];
  informationMode: FloatBarInformationMode;
  isCompact: boolean;
  expanded: boolean;
  onToggleExpand: () => void;
}) {
  // Keep the number, label, and tone tied to the same displayed window.
  const hero = floatBarWindow(provider);
  const freshness = capacityFreshness(provider);
  const boosts = activePromoBoosts(provider);
  const remaining = Math.max(0, Math.min(100, hero.window.remainingPercent));
  const used = Math.max(0, Math.min(100, hero.window.usedPercent));
  const displayPercent = showAsUsed ? used : remaining;
  const displaySuffix = showAsUsed ? usedSuffix : remainingSuffix;
  const pressureRemaining = Math.max(
    0,
    Math.min(100, hero.window.remainingPercent),
  );
  const exhausted =
    !hero.namedState && (hero.window.isExhausted || !!provider.error);
  let tone: "ok" | "warn" | "crit" = "ok";
  const isHarness = provider.providerId === "deepseekharness";
  if (hero.namedState === "unavailable" && !isHarness) {
    // Caution/amber: the window was tracked but dropped out (CEILING_UI.md).
    // Do not treat remainingPercent 100 as healthy (SBS-876).
    tone = "warn";
  } else if (exhausted || pressureRemaining <= critRemaining) {
    tone = "crit";
  } else if (pressureRemaining <= highRemaining) {
    tone = "warn";
  }

  const brand = getProviderIcon(provider.providerId).brandColor;
  // A lane billed in currency leads with the money. "62%" of a spend cap is not
  // the number you act on — the amount owed is (SBS-191).
  const spend = hero.amount ? stripAmountLabel(hero.amount, showAsUsed) : null;
  const resetText = useFormattedResetTime(
    hero.window.resetsAt,
    hero.window.resetDescription,
    resetRelative,
  );
  const inlineReset = resetText ? inlineResetTime(resetText) : null;
  const { t } = useLocale();
  const namedLabel = isHarness
    ? provider.primary.resetDescription || "—"
    : hero.namedState === "unavailable"
      ? t("WindowUnavailable")
      : hero.namedState === "notEnforced"
        ? t("NotCurrentlyEnforced")
        : null;
  const label = provider.error
    ? "—"
    : (namedLabel ?? spend ?? `${Math.round(displayPercent)}%`);
  const iconSize = Math.round(14 * scale);
  const resetIconSize = Math.round(10 * scale);
  const stateChip = freshnessChipLabel(freshness, t);
  const boostTitle = boosts[0]?.title ?? null;
  // Strip always shows reset when depleted; otherwise honor the setting.
  // Named-state windows are not a quota, so do not promote a billing-cycle
  // reset as if the placeholder 0% had just run out (SBS-876).
  const showReset =
    !hero.namedState && !!inlineReset && (showResetInline || exhausted);
  const titleBits = [
    hero.namedState
      ? `${provider.displayName}: ${label}`
      : `${provider.displayName}: ${label} ${displaySuffix}`,
    hero.label,
    boostTitle ? `promo ${boostTitle}` : null,
    stateChip,
    hero.namedState ? null : resetText,
  ]
    .filter(Boolean)
    .join("\n");

  const pillClass = [
    "floatbar__pill",
    `floatbar__pill--${tone}`,
    freshness !== "live" ? `floatbar__pill--${freshness}` : null,
    capacityEventKind ? "floatbar__pill--capacity-event" : null,
    capacityEventKind ? `floatbar__pill--${capacityEventKind}` : null,
  ]
    .filter(Boolean)
    .join(" ");

  // Calm mode: lead with a trustworthy pace state and the next reset; exact %
  // stays one keyboard/click away. The pill becomes an expandable button, so it
  // opts out of the native drag region (drag the bar by its handle instead).
  if (informationMode === "calm") {
    // A named-state window is not a quota. Its pace and billing reset are both
    // derived from the placeholder 0%, so calm drops them and shows the same
    // named label exact mode does (SBS-876).
    const calm = hero.namedState
      ? { pace: null, hasReset: false }
      : calmPresentation(provider, hero);
    // Compact hides the window·reset row, so calm degrades to pace-or-exact.
    // Otherwise show exact only when there is no pace and no reset time to show.
    // Either way the pill is never blank.
    const showResetRow = !isCompact && calm.hasReset;
    const showExact =
      !!hero.namedState ||
      expanded ||
      (isCompact ? !calm.pace : !calm.pace && !inlineReset);
    const onKeyToggle = (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        onToggleExpand();
      }
    };
    // Announce what the pill actually shows, not the exact % it hides — screen
    // readers should hear the pace/reset state, and the % only once revealed.
    const calmLabel = [
      `${provider.displayName}:`,
      calm.pace?.label ?? null,
      showResetRow
        ? `${hero.label}${resetText ? ` ${resetText}` : ""}`
        : null,
      showExact
        ? hero.namedState
          ? label
          : `${label} ${displaySuffix}`
        : null,
      stateChip,
    ]
      .filter(Boolean)
      .join(" ");
    return (
      <div
        className={`${pillClass} floatbar__pill--calm${expanded ? " floatbar__pill--expanded" : ""}`}
        title={titleBits}
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        aria-label={calmLabel}
        onClick={onToggleExpand}
        onKeyDown={onKeyToggle}
        onMouseDown={(event) => event.stopPropagation()}
        style={{ "--brand": brand } as CSSProperties}
      >
        <span className="floatbar__provider-icon">
          <ProviderIcon providerId={provider.providerId} size={iconSize} />
        </span>
        <span className="floatbar__text">
          {calm.pace ? (
            <span className={`floatbar__pace floatbar__pace--${calm.pace.tone}`}>
              {calm.pace.label}
            </span>
          ) : null}
          {showResetRow ? (
            <span className="floatbar__calm-reset">
              <span className="floatbar__window">{hero.label}</span>
              {inlineReset ? (
                <>
                  <span className="floatbar__calm-sep" aria-hidden>
                    ·
                  </span>
                  <ResetIcon size={resetIconSize} />
                  <span className="floatbar__reset-time">{inlineReset}</span>
                </>
              ) : null}
            </span>
          ) : null}
          {showExact ? (
            <span className="floatbar__pct floatbar__pct--calm">{label}</span>
          ) : null}
        </span>
      </div>
    );
  }

  return (
    <div
      className={pillClass}
      title={titleBits}
      data-tauri-drag-region
      style={{ "--brand": brand } as CSSProperties}
    >
      <span className="floatbar__provider-icon" data-tauri-drag-region>
        <ProviderIcon providerId={provider.providerId} size={iconSize} />
      </span>
      <span className="floatbar__text" data-tauri-drag-region>
        <span className="floatbar__pct" data-tauri-drag-region>
          {label}
        </span>
        <span className="floatbar__window" data-tauri-drag-region>
          {hero.label}
        </span>
        {stateChip && (
          <span
            className={`floatbar__chip floatbar__chip--${freshness}`}
            data-tauri-drag-region
          >
            {stateChip}
          </span>
        )}
        {showReset && (
          <span
            className={`floatbar__reset${exhausted ? " floatbar__reset--emphasis" : ""}`}
            title={resetText ?? undefined}
            aria-label={resetText ?? undefined}
            data-tauri-drag-region
          >
            <ResetIcon size={resetIconSize} />
            <span className="floatbar__reset-time" data-tauri-drag-region>
              {inlineReset}
            </span>
          </span>
        )}
      </span>
    </div>
  );
}

function freshnessChipLabel(
  freshness: CapacityFreshness,
  t: (key: "FreshnessStale" | "FreshnessError") => string,
): string | null {
  switch (freshness) {
    case "stale":
      return t("FreshnessStale");
    case "error":
      return t("FreshnessError");
    case "live":
      return null;
  }
}

/**
 * The always-on-top floating capacity bar.
 *
 * Renders a tiny strip of provider pills. Listens to the same provider
 * refresh cycle as the rest of the app via `useProviders`, and reacts to
 * setting changes (filter list, orientation) live without a reload.
 */
/** localStorage key persisting the float bar's "lock in place" state. */
const FLOATBAR_LOCK_KEY = "ceiling.floatbar.locked";

function useSystemPrefersLight(): boolean {
  const query = "(prefers-color-scheme: light)";
  const [prefersLight, setPrefersLight] = useState(
    () => typeof window.matchMedia === "function" && window.matchMedia(query).matches,
  );

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const media = window.matchMedia(query);
    const update = (event: MediaQueryListEvent) => setPrefersLight(event.matches);
    setPrefersLight(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  return prefersLight;
}

export default function FloatBar({ state }: { state: BootstrapState }) {
  const { t } = useLocale();
  const systemPrefersLight = useSystemPrefersLight();
  const { providers } = useProviders({
    refreshOnMount: false,
  });
  const [locked, setLocked] = useState(
    () => localStorage.getItem(FLOATBAR_LOCK_KEY) === "1",
  );
  const lockedRef = useRef(locked);
  lockedRef.current = locked;
  // Right-click opens an in-place action row (see FloatBarMenu). Kept in a ref
  // too so the capture-phase drag guard can read it without re-subscribing.
  const [menuOpen, setMenuOpen] = useState(false);
  const menuOpenRef = useRef(menuOpen);
  menuOpenRef.current = menuOpen;
  // Calm-mode pills the user has expanded to reveal the exact percentage.
  const [expandedKeys, setExpandedKeys] = useState<Set<string>>(new Set());
  const toggleExpanded = useCallback((key: string) => {
    setExpandedKeys((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const startDrag = useCallback((event: MouseEvent<HTMLElement>) => {
    if (event.button !== 0 || lockedRef.current || menuOpenRef.current) return;
    void getCurrentWindow().startDragging().catch(() => {});
  }, []);

  const toggleLock = useCallback(() => {
    setLocked((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(FLOATBAR_LOCK_KEY, next ? "1" : "0");
      } catch {
        /* storage disabled — the lock just won't persist across restarts */
      }
      return next;
    });
  }, []);

  // Replace the webview's generic right-click menu with a purposeful action row.
  // While locked, the native `data-tauri-drag-region` drag is suppressed so the
  // bar can't be nudged; menu clicks are always let through.
  useEffect(() => {
    const onContextMenu = (event: globalThis.MouseEvent) => {
      event.preventDefault();
      setMenuOpen((prev) => !prev);
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    const onMouseDownCapture = (event: globalThis.MouseEvent) => {
      // Never eat clicks aimed at the action row.
      if ((event.target as HTMLElement | null)?.closest(".floatbar__menu")) {
        return;
      }
      if (lockedRef.current && event.button === 0) {
        // Pre-empt Tauri's drag-region handler before it starts a move.
        event.preventDefault();
        event.stopPropagation();
      }
    };
    document.addEventListener("contextmenu", onContextMenu);
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("mousedown", onMouseDownCapture, true);
    return () => {
      document.removeEventListener("contextmenu", onContextMenu);
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("mousedown", onMouseDownCapture, true);
    };
  }, []);

  // Mark the body so our CSS can strip the dark theme background — the
  // floatbar window is meant to be fully transparent around the pills.
  useEffect(() => {
    document.body.classList.add("floatbar-window");
    return () => {
      document.body.classList.remove("floatbar-window");
    };
  }, []);

  // The floatbar window is detached, so it doesn't share React state
  // with the Settings tab. Listen for the Rust-side config-changed event
  // and re-pull the snapshot when fired.
  const [settings, setSettings] = useState<SettingsSnapshot>(state.settings);
  const [lastActiveProviderId, setLastActiveProviderId] = useState<
    string | null
  >(null);
  const [capacityEvents, setCapacityEvents] = useState<
    Record<string, CapacityEventPayload["kind"]>
  >({});
  const capacityEventTimers = useRef<Record<string, number>>({});

  // The detached floatbar should keep usage fresh, but it must not open or
  // focus any other surface. Refresh data only; provider-updated events feed
  // this window when the backend completes. 0 = manual refresh only.
  useEffect(() => {
    const intervalSecs = settings.refreshIntervalSecs;
    if (intervalSecs === 0) {
      return;
    }
    const intervalMs = intervalSecs * 1000;
    const tick = () => {
      void refreshProvidersIfStale().catch(() => {});
    };
    tick();
    const id = setInterval(tick, intervalMs);
    return () => clearInterval(id);
  }, [settings.refreshIntervalSecs]);

  useEffect(() => {
    const unlisten = listen(FLOAT_BAR_CONFIG_CHANGED_EVENT, () => {
      void getSettingsSnapshot().then(setSettings).catch(() => {});
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    const mode = settings.floatBarSelectionMode ?? "pinned";
    if (mode === "pinned" || !settings.floatBarForegroundDetection) {
      return;
    }
    const apply = (snapshot: ForegroundProviderSnapshot) => {
      setLastActiveProviderId(snapshot.lastActiveProviderId);
    };
    void getForegroundProvider().then(apply).catch(() => {});
    const unlisten = listen<ForegroundProviderSnapshot>(
      "foreground-provider-changed",
      ({ payload }) => apply(payload),
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [settings.floatBarSelectionMode, settings.floatBarForegroundDetection]);

  useEffect(() => {
    const unlisten = listen<CapacityEventPayload>("capacity-event", ({ payload }) => {
      if (!settings.enableAnimations) return;
      const providerId = payload.providerId;
      const previousTimer = capacityEventTimers.current[providerId];
      if (previousTimer !== undefined) window.clearTimeout(previousTimer);
      setCapacityEvents((current) => ({ ...current, [providerId]: payload.kind }));
      capacityEventTimers.current[providerId] = window.setTimeout(() => {
        setCapacityEvents((current) => {
          const next = { ...current };
          delete next[providerId];
          return next;
        });
        delete capacityEventTimers.current[providerId];
      }, 2200);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [settings.enableAnimations]);

  useEffect(
    () => () => {
      for (const timer of Object.values(capacityEventTimers.current)) {
        window.clearTimeout(timer);
      }
    },
    [],
  );

  // Orientation flips re-lay-out the bar without recreating the window.
  const orientation: "horizontal" | "vertical" =
    settings.floatBarOrientation === "vertical" ? "vertical" : "horizontal";
  const style = settings.floatBarStyle === "taskbar" ? "taskbar" : "floating";
  const density = ["compact", "detailed"].includes(settings.floatBarDensity)
    ? settings.floatBarDensity
    : "standard";
  const contrast = settings.floatBarContrast ?? "light-text";
  const informationMode: FloatBarInformationMode =
    settings.floatBarInformationMode === "calm" ? "calm" : "exact";
  const darkText =
    contrast === "dark-text" || (contrast === "auto" && systemPrefersLight);
  const filterIds = settings.floatBarProviderIds;
  const selectionMode: FloatBarSelectionMode =
    settings.floatBarSelectionMode === "active" ||
    settings.floatBarSelectionMode === "activePlusCritical"
      ? settings.floatBarSelectionMode
      : "pinned";
  const scale = Math.max(0.75, Math.min(2, settings.floatBarScale / 100));
  const showResetInline = settings.floatBarShowResetInline || density === "detailed";
  const pinnedAccounts = settings.taskbarAccountByProvider ?? {};
  const visible = useMemo(() => {
    const enabled = new Set(settings.enabledProviders);
    // Enabled alone is not enough: skip not-yet-ready error placeholders so
    // turning on Antigravity without the app installed does not mint a blank
    // "Claude" pill on the taskbar.
    const eligible = providers.filter(
      (p) => enabled.has(p.providerId) && isFloatBarEligible(p),
    );
    // One pill per provider: pin wins, else hottest account.
    const byProvider = new Map<string, typeof eligible>();
    for (const row of eligible) {
      const group = byProvider.get(row.providerId) ?? [];
      group.push(row);
      byProvider.set(row.providerId, group);
    }
    const pick = (providerId: string) => {
      const group = byProvider.get(providerId) ?? [];
      if (group.length === 0) return undefined;
      const want = pinnedAccounts[providerId]?.trim();
      if (want) {
        const hit = group.find((row) => row.accountId === want);
        if (hit) return hit;
      }
      return [...group].sort((a, b) => {
        const delta =
          floatBarHeat(b) - floatBarHeat(a);
        if (delta !== 0) return delta;
        // Lowest account id, matching the native strip and the flyout so all
        // three name the same seat when two accounts read the same pressure.
        return compareAccountIds(a.accountId, b.accountId);
      })[0];
    };
    const pinned =
      filterIds && filterIds.length > 0
        ? filterIds
            .map((id) => pick(id))
            .filter((p): p is (typeof eligible)[number] => p !== undefined)
        : [...byProvider.keys()]
            .map((id) => pick(id))
            .filter((p): p is (typeof eligible)[number] => p !== undefined)
            .sort((a, b) => floatBarHeat(b) - floatBarHeat(a));
    const allEligible = [...byProvider.keys()]
      .map((id) => pick(id))
      .filter((p): p is (typeof eligible)[number] => p !== undefined);
    return selectVisibleFloatBarProviders(pinned, allEligible, {
      mode: selectionMode,
      detectionEnabled: settings.floatBarForegroundDetection,
      lastActiveProviderId,
      usedPercent: (row) => floatBarHeat(row),
      highUsageThreshold: settings.highUsageThreshold,
    });
  }, [
    providers,
    settings.enabledProviders,
    filterIds,
    pinnedAccounts,
    selectionMode,
    settings.floatBarForegroundDetection,
    lastActiveProviderId,
    settings.highUsageThreshold,
  ]);

  // Keep the native floatbar window fitted when late data/fonts/icons change layout.
  const lastResizeRef = useRef<{ w: number; h: number } | null>(null);
  const resizeRafRef = useRef<number | null>(null);
  const resizeToContent = useCallback(() => {
    const el = document.querySelector<HTMLElement>(".floatbar");
    if (!el) return;
    if (resizeRafRef.current !== null) {
      cancelAnimationFrame(resizeRafRef.current);
    }
    resizeRafRef.current = requestAnimationFrame(() => {
      resizeRafRef.current = null;
      const rect = el.getBoundingClientRect();
      const padding = 8;
      const w = Math.ceil(rect.width + padding);
      const h = Math.ceil(rect.height + padding);
      const last = lastResizeRef.current;
      if (last && Math.abs(last.w - w) <= 1 && Math.abs(last.h - h) <= 1) return;
      lastResizeRef.current = { w, h };
      void resizeFloatBar(w, h).catch(() => {});
    });
  }, []);

  useEffect(() => {
    resizeToContent();
  }, [
    resizeToContent,
    visible.length,
    orientation,
    style,
    density,
    contrast,
    systemPrefersLight,
    scale,
    showResetInline,
    settings.resetTimeRelative,
    informationMode,
    expandedKeys,
    menuOpen,
  ]);

  useEffect(() => {
    const el = document.querySelector<HTMLElement>(".floatbar");
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(resizeToContent);
    observer.observe(el);
    return () => observer.disconnect();
  }, [resizeToContent]);

  useEffect(
    () => () => {
      if (resizeRafRef.current !== null) {
        cancelAnimationFrame(resizeRafRef.current);
      }
    },
    [],
  );

  const highRemaining = 100 - settings.highUsageThreshold;
  const critRemaining = 100 - settings.criticalUsageThreshold;
  const opacityFraction = Math.max(0.3, Math.min(1, settings.floatBarOpacity / 100));

  const handleToggleLock = useCallback(() => {
    toggleLock();
    setMenuOpen(false);
  }, [toggleLock]);
  const handleToggleClickThrough = useCallback(() => {
    const next = !settings.floatBarClickThrough;
    void setFloatBarClickThrough(next).catch(() => {});
    setMenuOpen(false);
  }, [settings.floatBarClickThrough]);
  const handleOpenSettings = useCallback(() => {
    void openSettingsWindow("menu").catch(() => {});
    setMenuOpen(false);
  }, []);
  const handleHide = useCallback(() => {
    void hideFloatBar().catch(() => {});
    setMenuOpen(false);
  }, []);

  return (
    <div
      className={`floatbar floatbar--${orientation} floatbar--${style} floatbar--density-${density} floatbar--mode-${informationMode}${darkText ? " floatbar--light-bg" : ""}${locked ? " floatbar--locked" : ""}${menuOpen ? " floatbar--menu-open" : ""}`}
      data-tauri-drag-region
      onMouseDown={startDrag}
      style={
        {
          opacity: opacityFraction,
          "--floatbar-scale": scale,
        } as CSSProperties
      }
    >
      {menuOpen ? (
        <FloatBarMenu
          locked={locked}
          clickThrough={settings.floatBarClickThrough}
          onToggleLock={handleToggleLock}
          onToggleClickThrough={handleToggleClickThrough}
          onOpenSettings={handleOpenSettings}
          onHide={handleHide}
        />
      ) : visible.length === 0 ? (
        <div className="floatbar__empty" data-tauri-drag-region>
          {t("FloatBarNoProviders")}
        </div>
      ) : (
        // One unified glass capsule; each provider is a segment split by a
        // hairline divider, with the drag grip on the left. Reads as a single
        // premium widget instead of scattered pills.
        <div className="floatbar__bar" data-tauri-drag-region>
          <div className="floatbar__handle" data-tauri-drag-region aria-hidden />
          {locked && (
            <span
              className="floatbar__lock"
              aria-hidden
              title="Locked in place — right-click to unlock"
            >
              <svg
                viewBox="0 0 24 24"
                width="11"
                height="11"
                fill="none"
                stroke="currentColor"
                strokeWidth={2.2}
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <rect x="5" y="11" width="14" height="9" rx="2" />
                <path d="M8 11V8a4 4 0 0 1 8 0v3" />
              </svg>
            </span>
          )}
          {visible.map((p) => (
            <ProviderPill
              key={providerKey(p)}
              provider={p}
              highRemaining={highRemaining}
              critRemaining={critRemaining}
              showAsUsed={settings.showAsUsed}
              scale={scale}
              showResetInline={showResetInline}
              resetRelative={settings.resetTimeRelative}
              usedSuffix={t("PanelUsedSuffix")}
              remainingSuffix={t("FloatBarRemainingSuffix")}
              capacityEventKind={capacityEvents[p.providerId]}
              informationMode={informationMode}
              isCompact={density === "compact"}
              expanded={expandedKeys.has(providerKey(p))}
              onToggleExpand={() => toggleExpanded(providerKey(p))}
            />
          ))}
        </div>
      )}
    </div>
  );
}
