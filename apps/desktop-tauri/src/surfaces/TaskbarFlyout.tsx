import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { CeilingMark } from "../components/CeilingMark";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import { useProviders } from "../hooks/useProviders";
import { useSettings } from "../hooks/useSettings";
import { getProviderIcon } from "../components/providers/providerIcons";
import { orderProviderSnapshots } from "../lib/providerOrder";
import { useLocale } from "../hooks/useLocale";
import {
  allMeasuredWindows,
  bankedResetCredits,
  type ConstrainingWindow,
  type NamedWindowState,
} from "../lib/capacityPresentation";
import { formatResetCountdown } from "../lib/resetCountdown";
import {
  dismissTrayPanel,
  getTaskbarSurfaceColor,
  reanchorTrayPanel,
  revealTrayPanelWindow,
  setSurfaceMode,
} from "../lib/tauri";
import type { BootstrapState, ProviderUsageSnapshot, RateWindowSnapshot } from "../types/bridge";
import {
  accountIdentityLabel,
  hasMultipleAccounts,
  orderFlyoutProviders,
  providerRowKey,
  selectStripAccount,
} from "../lib/providerRow";

const FLYOUT_WIDTH = 344;
// Mirrors `FLYOUT_INITIAL_HEIGHT` in `shell/flyout_window.rs`. Used only when
// the section has no layout to measure yet — never as a floor. Everything
// outside `.taskbar-flyout` is transparent, so a window taller than the section
// shows the desktop as a bare strip under the footer: header + one Unavailable
// row + footer measures 163px, which the old `Math.max(174, …)` padded to 174.
const FLYOUT_FALLBACK_HEIGHT = 174;
const MAX_VISIBLE_PROVIDERS = 6;
const MAX_VISIBLE_WINDOWS_PER_PROVIDER = 4;

function compactDuration(resetsAt: string | null, fallback: string | null, now: number): string {
  if (!resetsAt) return fallback?.replace(/^resets?\s+(in\s+)?/i, "") ?? "No reset";
  const target = Date.parse(resetsAt);
  if (!Number.isFinite(target)) return fallback ?? "No reset";
  // Floor-and-clamp, same as tray/CLI/tooltip. The old ceil made 61-119s
  // read "2m" while every other surface said "1m" (SBS-927).
  return formatResetCountdown(target - now) ?? "Now";
}

function valueFor(window: RateWindowSnapshot, showAsUsed: boolean): number {
  return Math.max(0, Math.min(100, Math.round(showAsUsed ? window.usedPercent : window.remainingPercent)));
}

function meterLevel(window: RateWindowSnapshot): "normal" | "warning" | "critical" {
  if (window.usedPercent >= 95) return "critical";
  if (window.usedPercent >= 85) return "warning";
  return "normal";
}

function earliestReset(providers: ProviderUsageSnapshot[], now: number): string | null {
  let earliest: string | null = null;
  let earliestTime = Number.POSITIVE_INFINITY;
  for (const provider of providers) {
    if (provider.error) continue;
    const windows = allMeasuredWindows(provider).filter(isResetClockWindow);
    for (const { window } of windows) {
      const candidate = window?.resetsAt;
      if (!candidate) continue;
      const time = Date.parse(candidate);
      if (Number.isFinite(time) && time > now && time < earliestTime) {
        earliest = candidate;
        earliestTime = time;
      }
    }
  }
  return earliest;
}

/** Promotional grants are giveaways, not limits — they never earn a row. */
function isMeteredWindow(window: ConstrainingWindow): boolean {
  return !`${window.id} ${window.label}`.toLowerCase().includes("promotional");
}

/**
 * Windows that answer "when does capacity come back".
 *
 * A spend lane bills rather than blocks, so it stays out of the header clock —
 * but it is still a real row. Folding both questions into one predicate is what
 * erased On-demand from the flyout entirely (SBS-191).
 */
function isResetClockWindow(window: ConstrainingWindow): boolean {
  const identity = `${window.id} ${window.label}`.toLowerCase();
  return (
    isMeteredWindow(window) &&
    !["on-demand", "on demand", "ondemand"].some((lane) =>
      identity.includes(lane),
    )
  );
}

function namedStateRows(provider: ProviderUsageSnapshot): ConstrainingWindow[] {
  return (provider.inactiveRateWindows ?? []).map((row) => ({
    id: `inactive-${row.id}`,
    label: row.title.trim() || row.id,
    // Percent is not painted for named-state rows; the primary snapshot is
    // only here to satisfy ConstrainingWindow (SBS-876).
    window: provider.primary,
    namedState: (row.state ?? "notEnforced") as NamedWindowState,
  }));
}

function flyoutWindows(provider: ProviderUsageSnapshot): ConstrainingWindow[] {
  // allMeasuredWindows already omits a placeholder primary (SBS-876).
  const windows = allMeasuredWindows(provider).filter(isMeteredWindow);
  const named = namedStateRows(provider);
  if (provider.providerId !== "cursor") {
    return [...windows, ...named].slice(0, MAX_VISIBLE_WINDOWS_PER_PROVIDER);
  }

  // Cursor's durable allowances plus the lane that actually charges you.
  // On-demand is pinned rather than left to fill a leftover slot: when the
  // other three are depleted it is the only row that still means anything.
  // A placeholder Plan is not in `windows`; it arrives as a named-state row, so
  // keeping `primary` in the preference list costs nothing when it is missing
  // and stops a real Plan reading from being pushed past the visible slots.
  const preferredIds = [
    "primary",
    "secondary",
    "extra-cursor-api",
    "extra-cursor-on-demand",
  ];
  const preferred = preferredIds
    .map((id) => windows.find((window) => window.id === id))
    .filter((window): window is ConstrainingWindow => Boolean(window));
  const remaining = windows.filter(
    (window) => !preferred.some((candidate) => candidate.id === window.id),
  );
  return [...preferred, ...remaining, ...named].slice(
    0,
    MAX_VISIBLE_WINDOWS_PER_PROVIDER,
  );
}

/**
 * Every row `flyoutWindows` had to choose from, before the visible-row cap.
 *
 * The "more limits" note is the only sign that something was cut, so it has to
 * count named-state rows too. Measuring hidden rows against the measured
 * windows alone hid truncated Unavailable rows entirely, and once named rows
 * filled the visible slots the subtraction went negative and reported nothing
 * hidden at all (SBS-876).
 */
function flyoutCandidateCount(provider: ProviderUsageSnapshot): number {
  return (
    allMeasuredWindows(provider).filter(isMeteredWindow).length +
    namedStateRows(provider).length
  );
}

function ProviderRow({ provider, showAccount, hideEmail, onStrip, showAsUsed, now }: {
  provider: ProviderUsageSnapshot;
  // True when this provider has more than one account, so the account name is
  // needed to tell its rows apart. With one account it would be noise.
  showAccount: boolean;
  hideEmail: boolean;
  /** This row is the account currently driving the compact strip tile. */
  onStrip: boolean;
  showAsUsed: boolean;
  now: number;
}) {
  const { t } = useLocale();
  const accountName = showAccount
    ? accountIdentityLabel(provider, hideEmail)
    : null;
  const icon = getProviderIcon(provider.providerId);
  if (provider.error) {
    return (
      <div
        className={`taskbar-flyout__provider taskbar-flyout__provider--error${onStrip ? " taskbar-flyout__provider--on-strip" : ""}`}
        style={{ "--provider-brand": icon.brandColor } as CSSProperties}
      >
        <ProviderIcon providerId={provider.providerId} size={27} className="taskbar-flyout__provider-icon" />
        <div className="taskbar-flyout__provider-content">
          <div className="taskbar-flyout__provider-topline">
            <span className="taskbar-flyout__provider-name">{provider.displayName}</span>
            <span className="taskbar-flyout__provider-unavailable">Unavailable</span>
            {onStrip && (
              <span className="taskbar-flyout__on-strip">On strip</span>
            )}
          </div>
          {accountName && (
            <div className="taskbar-flyout__provider-account" title={accountName}>
              {accountName}
            </div>
          )}
          <div className="taskbar-flyout__provider-status">Last sync failed · open Ceiling for details</div>
        </div>
      </div>
    );
  }
  const windows = flyoutWindows(provider);
  const resetCredits = bankedResetCredits(provider);
  const hiddenWindowCount = Math.max(
    0,
    flyoutCandidateCount(provider) - windows.length,
  );
  return (
    <div
      className={`taskbar-flyout__provider${onStrip ? " taskbar-flyout__provider--on-strip" : ""}`}
      style={{ "--provider-brand": icon.brandColor } as CSSProperties}
    >
      <ProviderIcon providerId={provider.providerId} size={27} className="taskbar-flyout__provider-icon" />
      <div className="taskbar-flyout__provider-content">
        <div className="taskbar-flyout__provider-topline">
          <span className="taskbar-flyout__provider-name">{provider.displayName}</span>
          {/* Banked resets sit next to the name they belong to; the strip marker
              holds the right edge so it lines up down the whole flyout. */}
          {resetCredits != null && (
            <span
              className={`taskbar-flyout__reset-credit${resetCredits === 0 ? " taskbar-flyout__reset-credit--empty" : ""}`}
            >
              ↻ {resetCredits} {resetCredits === 1 ? "reset ready" : "resets ready"}
            </span>
          )}
          {onStrip && (
            <span className="taskbar-flyout__on-strip">On strip</span>
          )}
        </div>
        {accountName && (
          <div className="taskbar-flyout__provider-account" title={accountName}>
            {accountName}
          </div>
        )}
        <div className="taskbar-flyout__meters">
          {windows.map(({ id, label, window, amount, namedState }) => {
            if (namedState) {
              const unavailable = namedState === "unavailable";
              return (
                <div
                  className={`taskbar-flyout__inactive${unavailable ? " taskbar-flyout__inactive--unavailable" : ""}`}
                  key={id}
                >
                  <span className="taskbar-flyout__meter-label">{label}</span>
                  <span>
                    {unavailable
                      ? t("WindowUnavailable")
                      : t("NotCurrentlyEnforced")}
                  </span>
                </div>
              );
            }
            const percent = valueFor(window, showAsUsed);
            const reset = compactDuration(window.resetsAt, window.resetDescription, now);
            const level = meterLevel(window);
            // A lane billed in currency reads as money first; the percentage is
            // only the shape of the bar beneath it.
            //
            // Deliberately not switched by `showAsUsed`. A row with space
            // states both numbers, so remaining is already on screen and the
            // toggle has nothing to reveal; only the one-number strips (the
            // taskbar tile and the floating bar) have to choose, and they
            // follow the setting. This matches `PlanStatusCard`, where the
            // percentage flips and the amount does not — making the flyout obey
            // the toggle would put it out of step with the Overview it summarizes.
            const spend = amount
              ? amount.formattedLimit
                ? `${amount.formattedUsed} of ${amount.formattedLimit}`
                : amount.formattedUsed
              : null;
            return (
              <div className="taskbar-flyout__meter" key={id} data-level={level}>
                <div className="taskbar-flyout__meter-meta">
                  <span className="taskbar-flyout__meter-label">{label}</span>
                  <span className="taskbar-flyout__meter-value" data-level={level}>{percent}%</span>
                  <span className="taskbar-flyout__reset">{reset}</span>
                </div>
                {spend && (
                  <div className="taskbar-flyout__meter-amount">{spend}</div>
                )}
                <div
                  className="taskbar-flyout__track"
                  data-level={level}
                  role="progressbar"
                  aria-label={
                    spend
                      ? `${provider.displayName} ${label} ${percent}% — ${spend}`
                      : `${provider.displayName} ${label} ${percent}%`
                  }
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={percent}
                >
                  <span style={{ width: `${percent}%` }} />
                </div>
              </div>
            );
          })}
          {hiddenWindowCount > 0 && (
            <div className="taskbar-flyout__window-more">+{hiddenWindowCount} more limits in Ceiling</div>
          )}
        </div>
      </div>
    </div>
  );
}

export default function TaskbarFlyout({ state }: { state: BootstrapState }) {
  const { settings } = useSettings(state.settings);
  const { providers, hasLoadedCache } = useProviders({ initialRefreshDelayMs: 800 });
  const [now, setNow] = useState(Date.now());
  const [surfaceColor, setSurfaceColor] = useState<string | null>(null);
  const surfaceRef = useRef<HTMLElement>(null);

  const taskbarProviders = useMemo(() => {
    const ordered = orderProviderSnapshots(
      providers,
      state.providers,
      settings.enabledProviders,
      settings.providerOrder,
    );
    const selected = settings.floatBarProviderIds ?? [];
    const filtered =
      selected.length === 0
        ? ordered
        : ordered.filter((provider) => selected.includes(provider.providerId));
    // Strip account first within each multi-account provider so the flyout
    // matches the tile the user is looking at.
    return orderFlyoutProviders(
      filtered,
      selected,
      settings.taskbarAccountByProvider ?? {},
    );
  }, [
    providers,
    settings.enabledProviders,
    settings.floatBarProviderIds,
    settings.providerOrder,
    settings.taskbarAccountByProvider,
    state.providers,
  ]);
  const visibleProviders = taskbarProviders.slice(0, MAX_VISIBLE_PROVIDERS);
  const hiddenProviderCount = Math.max(0, taskbarProviders.length - visibleProviders.length);
  const visibleWindowCount = visibleProviders.reduce(
    (total, provider) => total + flyoutWindows(provider).length,
    0,
  );

  const nextReset = earliestReset(taskbarProviders, now);
  const nextResetText = nextReset ? `Next reset in ${compactDuration(nextReset, null, now)}` : "Usage at a glance";

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    void getTaskbarSurfaceColor().then(setSurfaceColor).catch(() => {});
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!hasLoadedCache && visibleProviders.length === 0) return;
    let frame: number | null = null;
    const resize = () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        frame = null;
      // Windows owns the rounded outer edge; size directly to the content so
      // no transparent or CSS-border gutter can appear around that shape.
      const measured = Math.ceil(surfaceRef.current?.scrollHeight ?? 0);
      const height = measured > 0 ? measured : FLYOUT_FALLBACK_HEIGHT;
      void (async () => {
        await getCurrentWindow().setSize(new LogicalSize(FLYOUT_WIDTH, height)).catch(() => {});
        await reanchorTrayPanel().catch(() => {});
        await revealTrayPanelWindow().catch(() => {});
      })();
      });
    };
    resize();
    const surface = surfaceRef.current;
    const observer = surface && typeof ResizeObserver !== "undefined"
      ? new ResizeObserver(resize)
      : null;
    if (surface) observer?.observe(surface);
    return () => {
      observer?.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, [hasLoadedCache, hiddenProviderCount, visibleProviders.length, visibleWindowCount]);

  const openCeiling = useCallback(() => {
    void setSurfaceMode("popOut", { kind: "dashboard" })
      .then(() => dismissTrayPanel())
      .catch(() => {
        // Keep the flyout available if the dashboard could not be opened.
      });
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") void dismissTrayPanel().catch(() => {});
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    const surface = surfaceRef.current;
    if (!surface) return;
    let cleanupTimer: number | undefined;
    const replayEntrance = () => {
      surface.classList.remove("taskbar-flyout--entering");
      void surface.offsetWidth;
      surface.classList.add("taskbar-flyout--entering");
      window.clearTimeout(cleanupTimer);
      cleanupTimer = window.setTimeout(
        () => surface.classList.remove("taskbar-flyout--entering"),
        150,
      );
    };
    replayEntrance();
    window.addEventListener("focus", replayEntrance);
    return () => {
      window.removeEventListener("focus", replayEntrance);
      window.clearTimeout(cleanupTimer);
    };
  }, []);

  return (
    <main className="taskbar-flyout-frame" style={{ "--taskbar-surface": surfaceColor ?? "#073b78" } as CSSProperties}>
      <section className="taskbar-flyout" ref={surfaceRef} aria-label="Ceiling usage at a glance">
        <header className="taskbar-flyout__header">
          <CeilingMark size={32} appearance="glass" className="taskbar-flyout__mark" />
          <div>
            <div className="taskbar-flyout__title">Ceiling</div>
            <div className="taskbar-flyout__subtitle">{nextResetText}</div>
          </div>
        </header>

        <div className="taskbar-flyout__providers">
          {visibleProviders.map((provider) => {
            const multi = hasMultipleAccounts(
              taskbarProviders,
              provider.providerId,
            );
            const stripAccount = multi
              ? selectStripAccount(
                  taskbarProviders.filter(
                    (row) => row.providerId === provider.providerId,
                  ),
                  settings.taskbarAccountByProvider?.[provider.providerId],
                )
              : null;
            const onStrip =
              multi &&
              stripAccount != null &&
              providerRowKey(stripAccount) === providerRowKey(provider);
            return (
            <ProviderRow
              key={providerRowKey(provider)}
              provider={provider}
              showAccount={multi}
              hideEmail={settings.hidePersonalInfo}
              onStrip={onStrip}
              showAsUsed={settings.showAsUsed}
              now={now}
            />
            );
          })}
          {visibleProviders.length === 0 && (
            <div className="taskbar-flyout__empty">Syncing provider usage…</div>
          )}
          {hiddenProviderCount > 0 && (
            <div className="taskbar-flyout__more">+{hiddenProviderCount} more in Ceiling</div>
          )}
        </div>

        <button type="button" className="taskbar-flyout__open" onClick={openCeiling}>
          <svg viewBox="0 0 16 16" aria-hidden><path d="M6.5 3H3.8A1.8 1.8 0 0 0 2 4.8v7.4A1.8 1.8 0 0 0 3.8 14h7.4a1.8 1.8 0 0 0 1.8-1.8V9.5M9 2h5v5M8 8l6-6" /></svg>
          <span>Open Ceiling</span>
        </button>
      </section>
    </main>
  );
}
