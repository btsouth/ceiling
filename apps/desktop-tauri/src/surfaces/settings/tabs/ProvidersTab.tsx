import { useEffect, useMemo, useState } from "react";
import type {
  ProviderCatalogEntry,
  ProviderUsageSnapshot,
  SettingsUpdate,
} from "../../../types/bridge";
import type { BootstrapState } from "../../../types/bridge";
import { useLocale } from "../../../hooks/useLocale";
import type { LocaleKey } from "../../../i18n/keys";
import {
  ProvidersSidebar,
  type ProviderSidebarRow,
  type ProviderSidebarStatus,
} from "../providers/ProvidersSidebar";
import { ProviderDetailPane } from "../providers/ProviderDetailPane";
import { reorderProviders } from "../../../lib/tauri";
import { setDetectedProviderIgnored } from "../../../lib/detectedProviderPreferences";
import { useProviders } from "../../../hooks/useProviders";
import { representativeForProvider } from "../../../lib/providerRow";

interface ProvidersTabProps {
  settings: BootstrapState["settings"];
  providers: ProviderCatalogEntry[];
  set: (patch: SettingsUpdate) => void;
  saving: boolean;
}

export default function ProvidersTab({
  settings,
  providers,
  set,
  saving,
}: ProvidersTabProps) {
  const { t } = useLocale();
  const { providers: snapshots } = useProviders();
  const [selectedId, setSelectedId] = useState<string | null>(
    providers[0]?.id ?? null,
  );
  // Locally-owned catalog order so drag-reorder feels instant before the
  // backend `reorder_providers` round-trip settles.
  const [orderedProviders, setOrderedProviders] =
    useState<ProviderCatalogEntry[]>(providers);
  const [searchText, setSearchText] = useState("");

  useEffect(() => {
    setOrderedProviders(providers);
  }, [providers]);

  const enabled = useMemo(
    () => new Set(settings.enabledProviders),
    [settings.enabledProviders],
  );

  const toggle = (id: string, on: boolean) => {
    const next = new Set(enabled);
    if (on) next.add(id);
    else next.delete(id);
    setDetectedProviderIgnored(id, !on);
    set({
      enabledProviders: orderedProviders
        .map((provider) => provider.id)
        .filter((providerId) => next.has(providerId)),
    });
  };

  // Enabled providers first, so the ones you actually use are not buried
  // among two dozen you do not. Drag order is preserved *within* the enabled
  // group rather than alphabetised, because `reorder_providers` writes
  // `provider_order`, which is also the order cards appear in the tray flyout
  // and pop-out: re-sorting here would silently undo a deliberate dashboard
  // arrangement. The disabled tail has no such meaning, so it sorts by name.
  const sortedProviders = useMemo(
    () => sortProvidersForSidebar(orderedProviders, enabled),
    [enabled, orderedProviders],
  );

  const rows: ProviderSidebarRow[] = useMemo(() => {
    return sortedProviders.map((p) => {
      const isOn = enabled.has(p.id);
      // This list configures providers, not accounts, so each row still
      // summarises one snapshot. With several accounts that has to be a
      // deliberate choice rather than whichever the Map saw last.
      const snap = representativeForProvider(snapshots, p.id);
      return {
        id: p.id,
        displayName: p.displayName,
        enabled: isOn,
        status: deriveProviderStatus(isOn, snap),
        subtitlePrimary: providerSidebarSubtitle(p.id, isOn, snap, t),
        subtitleSecondary: providerSidebarMetric(snap),
      };
    });
  }, [enabled, snapshots, sortedProviders, t]);

  const normalizedSearch = searchText.trim().toLowerCase();
  const visibleRows = useMemo(
    () =>
      normalizedSearch
        ? rows.filter((row) => {
            const name = row.displayName.toLowerCase();
            const id = row.id.toLowerCase();
            return name.includes(normalizedSearch) || id.includes(normalizedSearch);
          })
        : rows,
    [normalizedSearch, rows],
  );

  useEffect(() => {
    if (visibleRows.length === 0) {
      if (selectedId !== null) setSelectedId(null);
      return;
    }
    if (!selectedId || !visibleRows.some((row) => row.id === selectedId)) {
      setSelectedId(visibleRows[0].id);
    }
  }, [selectedId, visibleRows]);

  const handleReorder = (ids: string[]) => {
    const byId = new Map(sortedProviders.map((p) => [p.id, p]));
    // Merge against the order actually on screen. Using the raw catalog order
    // here would drop the dragged row at the position it occupies in a list
    // the user is not looking at.
    const nextIds = normalizedSearch
      ? mergeFilteredOrder(
          sortedProviders.map((p) => p.id),
          new Set(visibleRows.map((row) => row.id)),
          ids,
        )
      : ids;
    const next = nextIds
      .map((id) => byId.get(id))
      .filter((p): p is ProviderCatalogEntry => Boolean(p));
    setOrderedProviders(next);
    void reorderProviders(nextIds).catch(() => {
      setOrderedProviders(providers);
    });
  };

  const selectedEntry =
    orderedProviders.find((p) => p.id === selectedId) ?? null;

  return (
    <div className="provider-split">
      <ProvidersSidebar
        providers={visibleRows}
        selectedId={selectedId}
        searchText={searchText}
        onSearchTextChange={setSearchText}
        onSelect={setSelectedId}
        onReorder={handleReorder}
        onToggleEnabled={toggle}
        disabled={saving}
      />
      <ProviderDetailPane
        providerId={selectedId}
        cookieDomain={selectedEntry?.cookieDomain ?? null}
        resetTimeRelative={settings.resetTimeRelative}
        providerMetrics={settings.providerMetrics}
        wayfinderGatewayUrl={settings.wayfinderGatewayUrl ?? "http://127.0.0.1:8088"}
        settingsDisabled={saving}
        onSettingsChange={set}
      />
    </div>
  );
}

/**
 * Sidebar order: enabled providers first, then the rest by name.
 *
 * Drag order is preserved *within* the enabled group rather than alphabetised,
 * because `reorder_providers` writes `provider_order`, which is also the order
 * cards appear in the tray flyout and pop-out: re-sorting here would silently
 * undo a deliberate dashboard arrangement. The disabled tail has no such
 * meaning, so it sorts by name.
 *
 * Must be idempotent: the list is re-sorted from persisted order on every
 * render, so a rule that reorders its own output would make rows drift.
 */
export function sortProvidersForSidebar<
  T extends { id: string; displayName: string },
>(providers: T[], enabled: Set<string>): T[] {
  const withIndex = providers.map((provider, index) => ({ provider, index }));
  withIndex.sort((a, b) => {
    const aOn = enabled.has(a.provider.id);
    const bOn = enabled.has(b.provider.id);
    if (aOn !== bOn) return aOn ? -1 : 1;
    if (aOn) return a.index - b.index;
    return a.provider.displayName.localeCompare(b.provider.displayName);
  });
  return withIndex.map((entry) => entry.provider);
}

function mergeFilteredOrder(
  fullOrder: string[],
  visibleIds: Set<string>,
  reorderedVisibleIds: string[],
): string[] {
  const nextVisible = [...reorderedVisibleIds];
  return fullOrder.map((id) =>
    visibleIds.has(id) ? (nextVisible.shift() ?? id) : id,
  );
}

// ── Provider sidebar subtitle helpers (port of
//    rust/src/native_ui/preferences.rs::provider_sidebar_subtitle). ─────

function deriveProviderStatus(
  isEnabled: boolean,
  snap: ProviderUsageSnapshot | null,
): ProviderSidebarStatus {
  if (!isEnabled) return "disabled";
  if (!snap) return "loading";
  if (snap.error) return "error";
  const updatedMs = new Date(snap.updatedAt).getTime();
  if (Number.isFinite(updatedMs)) {
    const ageMins = (Date.now() - updatedMs) / 60_000;
    if (ageMins > 10) return "stale";
  }
  return "ok";
}

/**
 * Minimal port of `provider_sidebar_source_hint`
 * (rust/src/native_ui/preferences.rs:3753). When we have a live snapshot the
 * backend-supplied `sourceLabel` wins; otherwise we fall back to the neutral
 * "Not detected" / "Disabled" copy.
 */
function providerSidebarSubtitle(
  providerId: string,
  isEnabled: boolean,
  snap: ProviderUsageSnapshot | null,
  t: (key: LocaleKey) => string,
): string {
  if (!isEnabled) {
    return `${t("ProviderDisabled")} — ${providerSourceHintShort(providerId, t)}`;
  }
  if (!snap) {
    return "Waiting for usage";
  }
  const source = snap.sourceLabel || providerSourceHintShort(providerId, t);
  return source;
}

function providerSourceHintShort(
  providerId: string,
  t: (key: LocaleKey) => string,
): string {
  const id = providerId.toLowerCase();
  switch (id) {
    case "cursor":
    case "factory":
    case "droid":
    case "kimi":
    case "kimik2":
    case "augment":
    case "opencode":
    case "amp":
    case "ollama":
    case "alibaba":
    case "infini":
    case "manus":
    case "mimo":
    case "commandcode":
      return t("ProviderSourceWebShort");
    case "gemini":
    case "antigravity":
    case "jetbrains":
      return t("ProviderSourceCliShort");
    case "copilot":
      return t("ProviderSourceOauthShort");
    case "zai":
    case "vertexai":
    case "openrouter":
    case "bedrock":
    case "nanogpt":
    case "warp":
    case "doubao":
    case "crof":
    case "stepfun":
    case "venice":
    case "openaiapi":
    case "elevenlabs":
    case "deepgram":
    case "groq":
    case "llmproxy":
      return t("ProviderSourceApiShort");
    case "kiro":
      return t("ProviderSourceKiroEnvShort");
    case "claude":
    case "codex":
    case "minimax":
    default:
      return t("ProviderSourceAutoShort");
  }
}

function providerSidebarMetric(
  snap: ProviderUsageSnapshot | null,
): string | undefined {
  if (!snap) return undefined;
  const rate = snap.primary;
  if (!rate) return undefined;
  if (snap.providerId === "deepseekharness") {
    return snap.error ? undefined : rate.resetDescription ?? undefined;
  }
  if (Number.isFinite(rate.usedPercent)) {
    return `${Math.round(Math.max(0, rate.usedPercent))}%`;
  }
  return undefined;
}
