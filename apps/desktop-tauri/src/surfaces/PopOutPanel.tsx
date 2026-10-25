import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type {
  BootstrapState,
  ProviderUsageSnapshot,
  ThemePreference,
} from "../types/bridge";
import { openSettingsWindow, quitApp as quitApplication } from "../lib/tauri";
import {
  hasMultipleAccounts,
  providerRowKey,
  representativeForProvider,
} from "../lib/providerRow";
import { useProviders } from "../hooks/useProviders";
import { useSettings } from "../hooks/useSettings";
import { useUpdateState } from "../hooks/useUpdateState";
import { useLocale } from "../hooks/useLocale";
import PlanStatusCard from "../components/PlanStatusCard";
import PopOutTitleBar from "../components/PopOutTitleBar";
import ActivityTimeline from "./ActivityTimeline";
import ChartsPanel from "./ChartsPanel";
import AccountsPanel from "./AccountsPanel";
import ProviderDetailView from "./ProviderDetailView";
import { MenuEmpty } from "../components/MenuSurface";
import {
  FirstRunChecklist,
  firstRunDismissed,
  dismissFirstRun,
} from "../components/FirstRunChecklist";
import UpdateBanner from "../components/UpdateBanner";
import DetectedAccountsCard from "../components/DetectedAccountsCard";
import { orderProviderSnapshots } from "../lib/providerOrder";
import { formatRelativeUpdated } from "../lib/relativeTime";
import { Dropdown } from "../components/Dropdown";

type DashboardSection = "overview" | "activity" | "accounts" | "charts";

const RAIL_ICON = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

function IconGrid() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...RAIL_ICON}>
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </svg>
  );
}
function IconClock() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...RAIL_ICON}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </svg>
  );
}
function IconUsers() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...RAIL_ICON}>
      <circle cx="9" cy="8" r="3.2" />
      <path d="M3.5 20a5.5 5.5 0 0 1 11 0" />
      <path d="M16 5.2a3.2 3.2 0 0 1 0 5.6" />
      <path d="M17.5 20a5.5 5.5 0 0 0-2.3-4.4" />
    </svg>
  );
}
function IconBars() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...RAIL_ICON}>
      <path d="M6 20V10" />
      <path d="M12 20V4" />
      <path d="M18 20v-7" />
    </svg>
  );
}
function IconGear() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...RAIL_ICON}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}
/**
 * Pop-out window — a windowed dashboard: left nav rail, an Overview of
 * plan-status glance cards (selecting a provider expands its full MenuCard),
 * and a bottom status bar. Mirrors the promo chrome.
 */
export default function PopOutPanel({
  state,
  providerId,
}: {
  state: BootstrapState;
  providerId?: string;
}) {
  const {
    providers,
    isRefreshing,
    refreshingProviderIds,
    refresh,
    hasCachedData,
  } = useProviders();
  const { settings, update } = useSettings(state.settings);
  const { updateState, checkNow, download, apply, dismiss, openRelease } =
    useUpdateState();
  const { t } = useLocale();

  const sorted = useMemo(() => {
    return orderProviderSnapshots(
      providers,
      state.providers,
      settings.enabledProviders,
      settings.providerOrder,
    );
  }, [providers, settings.enabledProviders, settings.providerOrder, state.providers]);
  const cachedProviderIds = useMemo(
    () => providers.map((provider) => provider.providerId),
    [providers],
  );
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(
    providerId ?? null,
  );
  const dashboardBodyRef = useRef<HTMLDivElement>(null);
  const windowScale = useMemo(() => {
    const scalePercent = Number(settings.windowScalePercent);
    return (
      Math.min(250, Math.max(100, Number.isFinite(scalePercent) ? scalePercent : 100)) / 100
    );
  }, [settings.windowScalePercent]);

  // Live "updated N ago" clock for the status bar; re-renders every 30s so the
  // relative time stays current without a refresh.
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, []);
  const latestUpdatedMs = useMemo(() => {
    let latest: number | null = null;
    for (const p of sorted) {
      if (!p.updatedAt) continue;
      const ms = Date.parse(p.updatedAt);
      if (!Number.isNaN(ms) && (latest === null || ms > latest)) latest = ms;
    }
    return latest;
  }, [sorted]);

  // Scale the dashboard via the webview's native zoom (like a browser's Ctrl-+):
  // it reflows content at the real window width, so the side-by-side cards keep
  // filling the window at any scale — unlike CSS `zoom`, which overflows. The
  // main window is shared with the tray surface, so reset zoom to 1 on unmount.
  useEffect(() => {
    const webview = getCurrentWebviewWindow();
    void webview.setZoom(windowScale).catch(() => {});
    return () => {
      void webview.setZoom(1).catch(() => {});
    };
  }, [windowScale]);

  useEffect(() => {
    setSelectedProviderId(providerId ?? null);
  }, [providerId]);

  useEffect(() => {
    if (
      selectedProviderId !== null &&
      providers.length > 0 &&
      !sorted.some((provider) => provider.providerId === selectedProviderId)
    ) {
      setSelectedProviderId(null);
    }
  }, [providers.length, selectedProviderId, sorted]);

  // Which row is open, when the user clicked a specific card. Selection is
  // still tracked by provider because deep links name a provider, not an
  // account, so both are kept.
  const [selectedRowKey, setSelectedRowKey] = useState<string | null>(null);

  const selectedProvider = useMemo(() => {
    if (selectedProviderId === null) return null;
    // An exact row wins; `.find` by provider alone opened the first account no
    // matter which card was clicked, so clicking the second account showed the
    // first account's numbers under its name.
    const exact = sorted.find(
      (provider) =>
        providerRowKey(provider) === selectedRowKey &&
        provider.providerId === selectedProviderId,
    );
    if (exact) return exact;
    // Arrived by deep link, or the chosen row is gone: summarise the account
    // closest to its limit, as every other surface does.
    return representativeForProvider(sorted, selectedProviderId);
  }, [selectedProviderId, selectedRowKey, sorted]);

  const handleGridClick = useCallback((nextProviderId: string | null) => {
    if (nextProviderId === null) setSelectedRowKey(null);
    setSelectedProviderId(nextProviderId);
  }, []);

  const openSettings = useCallback(() => {
    openSettingsWindow("general");
  }, []);
  const [firstRunHidden, setFirstRunHidden] = useState(() =>
    firstRunDismissed(),
  );
  const handleDismissFirstRun = useCallback(() => {
    dismissFirstRun();
    setFirstRunHidden(true);
  }, []);
  const quitApp = useCallback(() => {
    void quitApplication();
  }, []);

  const enableDetectedProviders = useCallback(
    async (providerIds: string[]) => {
      const next = [...new Set([...settings.enabledProviders, ...providerIds])];
      await update({ enabledProviders: next });
      refresh();
    },
    [refresh, settings.enabledProviders, update],
  );

  const [activeSection, setActiveSection] = useState<DashboardSection>("overview");

  useEffect(() => {
    if (dashboardBodyRef.current) dashboardBodyRef.current.scrollTop = 0;
  }, [activeSection, selectedProviderId]);

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.shiftKey || e.altKey || e.metaKey) return;
      switch (e.key.toLowerCase()) {
        case "r":
          e.preventDefault();
          refresh();
          break;
        case ",":
          e.preventDefault();
          openSettings();
          break;
        case "q":
          e.preventDefault();
          quitApp();
          break;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [refresh, openSettings, quitApp]);

  const banner = (
    <UpdateBanner
      updateState={updateState}
      onCheck={checkNow}
      onDownload={download}
      onApply={apply}
      onDismiss={dismiss}
      onOpenRelease={openRelease}
    />
  );

  const railItems: { id: DashboardSection; label: string; icon: ReactElement }[] = [
    { id: "overview", label: "Overview", icon: <IconGrid /> },
    { id: "activity", label: "Activity", icon: <IconClock /> },
    { id: "accounts", label: "Accounts", icon: <IconUsers /> },
    { id: "charts", label: "Charts", icon: <IconBars /> },
  ];
  const sectionMeta: Record<
    DashboardSection,
    { title: string; sub: string; blurb: string }
  > = {
    overview: { title: "Overview", sub: "Usage at a glance", blurb: "" },
    activity: {
      title: "Activity",
      sub: "Reset schedule",
      blurb: "Your upcoming provider resets in chronological order.",
    },
    accounts: {
      title: "Accounts",
      sub: "Connected providers",
      blurb: "Manage the accounts and sources Ceiling reads from.",
    },
    charts: {
      title: "Charts",
      sub: "Usage over time",
      blurb: "Limits and local usage trends, per provider.",
    },
  };
  const meta = sectionMeta[activeSection];

  return (
    <div className="popout-scale-shell dashboard-shell">
      <PopOutTitleBar />
      <div className="dashboard">
        <nav className="dashboard-rail" aria-label="Sections">
          {railItems.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`dashboard-rail__btn${activeSection === item.id ? " dashboard-rail__btn--active" : ""}`}
              aria-label={item.label}
              aria-current={activeSection === item.id ? "page" : undefined}
              title={item.label}
              onClick={() => {
                setActiveSection(item.id);
                // Returning to Overview clears any focused provider so all
                // cards show again (the switcher used to do this).
                if (item.id === "overview") setSelectedProviderId(null);
              }}
            >
              {item.icon}
            </button>
          ))}
          <div className="dashboard-rail__spacer" />
          <button
            type="button"
            className="dashboard-rail__btn"
            aria-label={t("TooltipSettings")}
            title={t("TooltipSettings")}
            onClick={openSettings}
          >
            <IconGear />
          </button>
        </nav>

        <div className="dashboard-main">
          {activeSection === "overview" && selectedProvider ? (
            <header className="dashboard-header dashboard-header--provider">
              <button
                type="button"
                className="provider-focus__back"
                onClick={() => setSelectedProviderId(null)}
                aria-label="Back to Overview"
              >
                <span aria-hidden>‹</span>
                Overview
              </button>
            </header>
          ) : (
            <header className="dashboard-header">
              <div className="dashboard-header__title">{meta.title}</div>
              <div className="dashboard-header__sub">{meta.sub}</div>
            </header>
          )}

          <div className="dashboard-body" ref={dashboardBodyRef}>
            {banner}
            {activeSection === "overview" ? (
              selectedProvider ? (
                <ProviderDetailView
                  provider={selectedProvider}
                  resetTimeRelative={settings.resetTimeRelative}
                  showAsUsed={settings.showAsUsed}
                  isRefreshing={refreshingProviderIds.has(selectedProvider.providerId)}
                  hideEmail={settings.hidePersonalInfo}
                />
              ) : (
                <>
                <DetectedAccountsCard
                  enabledProviderIds={settings.enabledProviders}
                  previouslyTrackedProviderIds={cachedProviderIds}
                  onEnable={enableDetectedProviders}
                  onManage={() => openSettingsWindow("providers")}
                />
                {sorted.length === 0 ? (
                  isRefreshing && !hasCachedData ? (
                    <MenuEmpty isLoading onSettings={openSettings} />
                  ) : firstRunHidden ? (
                    <MenuEmpty isLoading={false} onSettings={openSettings} />
                  ) : (
                    <FirstRunChecklist
                      enabledCount={settings.enabledProviders.length}
                      hasWorkingAuth={providers.some(
                        (provider) =>
                          settings.enabledProviders.includes(
                            provider.providerId,
                          ) && !provider.error,
                      )}
                      floatbarEnabled={
                        settings.floatBarEnabled || settings.taskbarWidgetEnabled
                      }
                      onOpenProviders={() => openSettingsWindow("providers")}
                      onOpenDisplay={() => openSettingsWindow("menu")}
                      onDismiss={handleDismissFirstRun}
                      t={t}
                    />
                  )
                ) : (
                  <div
                    className={`menu-stack${selectedProviderId === null ? " menu-stack--overview" : ""}`}
                  >
                    {sorted.map((p, idx) => (
                      <Fragment key={providerRowKey(p)}>
                        {idx > 0 && <div className="menu-stack__sep" />}
                        <div className="menu-stack__item">
                          <PlanStatusCard
                            provider={p}
                            showAccount={hasMultipleAccounts(sorted, p.providerId)}
                            hideEmail={settings.hidePersonalInfo}
                            isRefreshing={refreshingProviderIds.has(p.providerId)}
                            resetTimeRelative={settings.resetTimeRelative}
                            showResetWhenExhausted={settings.showResetWhenExhausted}
                            showAsUsed={settings.showAsUsed}
                            onSelect={() => {
                              setSelectedRowKey(providerRowKey(p));
                              handleGridClick(p.providerId);
                            }}
                          />
                        </div>
                      </Fragment>
                    ))}
                  </div>
                )}
                </>
              )
            ) : activeSection === "activity" ? (
              <ActivityTimeline
                providers={sorted}
                hideEmail={settings.hidePersonalInfo}
              />
            ) : activeSection === "charts" ? (
              <ChartsPanel
                providers={sorted}
                hideEmail={settings.hidePersonalInfo}
              />
            ) : activeSection === "accounts" ? (
              <AccountsPanel
                providers={sorted}
                hideEmail={settings.hidePersonalInfo}
                onManage={() => openSettingsWindow("providers")}
              />
            ) : null}
          </div>

          <footer className="dashboard-status">
            <div className="dashboard-status__theme" title={t("ThemeHelper")}>
              <Dropdown
                value={settings.theme}
                options={[
                  { value: "auto", label: t("ThemeAutoOption") },
                  { value: "light", label: t("ThemeLightOption") },
                  { value: "dark", label: t("ThemeDarkOption") },
                ]}
                ariaLabel={t("ThemeLabel")}
                buttonClassName="dashboard-status__toggle"
                onChange={(value) => {
                  void update({ theme: value as ThemePreference });
                }}
              />
            </div>
            <span className="dashboard-status__center">
              <span
                className={`dashboard-status__dot${isRefreshing ? " dashboard-status__dot--busy" : ""}`}
                aria-hidden
              />
              {isRefreshing
                ? t("SummaryRefreshing")
                : formatRelativeUpdated(latestUpdatedMs, t, nowMs)}
            </span>
            <span>All times local</span>
          </footer>
        </div>
      </div>
    </div>
  );
}
