import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
  getSettingsSnapshot: vi.fn(),
  updateSettings: vi.fn(),
  getUpdateState: vi.fn(),
  checkForUpdates: vi.fn(),
  downloadUpdate: vi.fn(),
  applyUpdate: vi.fn(),
  dismissUpdate: vi.fn(),
  openReleasePage: vi.fn(),
  openFlyoutWindow: vi.fn(),
  openSettingsWindow: vi.fn(),
  quitApp: vi.fn(),
  getProviderChartData: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  getDetectedProviderAccounts: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
}));

const eventListeners = new Map<string, (event: unknown) => void>();

const windowMocks = vi.hoisted(() => {
  const setSize = vi.fn().mockResolvedValue(undefined);
  const setPosition = vi.fn().mockResolvedValue(undefined);
  const minimize = vi.fn().mockResolvedValue(undefined);
  const toggleMaximize = vi.fn().mockResolvedValue(undefined);
  const close = vi.fn().mockResolvedValue(undefined);
  const isMaximized = vi.fn().mockResolvedValue(false);
  const onResized = vi.fn().mockResolvedValue(() => {});
  return {
    setSize,
    setPosition,
    minimize,
    toggleMaximize,
    close,
    isMaximized,
    onResized,
    getCurrentWindow: vi.fn(() => ({
      setSize,
      setPosition,
      minimize,
      toggleMaximize,
      close,
      isMaximized,
      onResized,
    })),
    LogicalSize: class LogicalSize {
      constructor(
        public width: number,
        public height: number,
      ) {}
    },
    LogicalPosition: class LogicalPosition {
      constructor(
        public x: number,
        public y: number,
      ) {}
    },
  };
});

const webviewWindowMocks = vi.hoisted(() => {
  const setZoom = vi.fn().mockResolvedValue(undefined);
  return {
    setZoom,
    getCurrentWebviewWindow: vi.fn(() => ({ setZoom })),
  };
});

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);
vi.mock("@tauri-apps/api/window", () => windowMocks);
vi.mock("@tauri-apps/api/webviewWindow", () => webviewWindowMocks);

import PopOutPanel from "./PopOutPanel";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import { TEST_PROVIDER_CATALOG } from "../test/providerCatalog";
import type {
  BootstrapState,
  ProviderCatalogEntry,
  ProviderUsageSnapshot,
  SettingsSnapshot,
} from "../types/bridge";

function rateWindow(used: number) {
  return {
    usedPercent: used,
    remainingPercent: 100 - used,
    windowMinutes: null,
    resetsAt: null,
    resetDescription: null,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function provider(id: string, displayName: string, used = 20): ProviderUsageSnapshot {
  return {
    providerId: id,
    displayName,
    primary: rateWindow(used),
    primaryLabel: "Monthly",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "auto",
    updatedAt: "2026-05-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
  };
}

function settings(): SettingsSnapshot {
  return {
    enabledProviders: ["codex", "claude"],
    refreshIntervalSecs: 300,
    refreshAllProvidersOnMenuOpen: false,
    startAtLogin: false,
    startMinimized: false,
    showNotifications: true,
    capacityEventNotificationsEnabled: true,
    soundEnabled: true,
    soundVolume: 100,
    highUsageThreshold: 70,
    criticalUsageThreshold: 90,
    predictivePaceWarningEnabled: false,
      switcherShowsIcons: true,
    menuBarShowsHighestUsage: false,
    menuBarShowsPercent: false,
    showAsUsed: true,
    showAllTokenAccountsInMenu: false,
    enableAnimations: true,
    resetTimeRelative: true,
    showResetWhenExhausted: false,
    menuBarDisplayMode: "detailed",
    hidePersonalInfo: false,
    updateChannel: "stable",
    autoDownloadUpdates: false,
    installUpdatesOnQuit: false,
    globalShortcut: "Ctrl+Shift+U",
    codexCustomSessionsDirs: [],
    uiLanguage: "english",
    theme: "dark",
    windowScalePercent: 125,
    trayScalePercent: 100,
    powertoysStatusPipeEnabled: false,
    claudeAvoidKeychainPrompts: false,
    codexSparkUsageVisible: true,
    disableKeychainAccess: false,
    providerMetrics: {},
    floatBarEnabled: false,
    taskbarWidgetEnabled: true,
    taskbarWidgetAllMonitors: false,
    floatBarOpacity: 80,
    floatBarScale: 100,
    floatBarOrientation: "horizontal",
    floatBarStyle: "floating",
    taskbarWidgetOpenOnHover: true,
    floatBarDensity: "standard",
    floatBarInformationMode: "exact",
    floatBarSelectionMode: "pinned",
    floatBarForegroundDetection: true,
    floatBarContrast: "auto",
    floatBarClickThrough: false,
    floatBarProviderIds: [],
    taskbarAccountByProvider: {},
    floatBarDarkText: false,
    floatBarShowResetInline: false,
    floatBarShowCost: false,
  };
}

function bootstrap(
  catalog: ProviderCatalogEntry[] = [],
  settingsOverride: Partial<SettingsSnapshot> = {},
): BootstrapState {
  return {
    contractVersion: "v1",
    providers: catalog,
    settings: { ...settings(), ...settingsOverride },
  };
}

function renderPopOut(
  providers: ProviderUsageSnapshot[],
  providerId?: string,
  catalog: ProviderCatalogEntry[] = [],
  settingsOverride: Partial<SettingsSnapshot> = {},
) {
  tauriMocks.getCachedProviders.mockResolvedValue(providers);
  tauriMocks.getSettingsSnapshot.mockResolvedValue({
    ...settings(),
    ...settingsOverride,
  });
  return render(
    <LocaleProvider>
      <PopOutPanel
        state={bootstrap(catalog, settingsOverride)}
        providerId={providerId}
      />
    </LocaleProvider>,
  );
}

describe("PopOutPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventListeners.clear();
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
    tauriMocks.updateSettings.mockResolvedValue(settings());
    tauriMocks.getDetectedProviderAccounts.mockResolvedValue([]);
    tauriMocks.getUpdateState.mockResolvedValue({
      status: "idle",
      version: null,
      error: null,
      progress: null,
      releaseUrl: null,
      canDownload: false,
      canApply: false,
      lastCheckedAt: null,
    });
    tauriMocks.getProviderChartData.mockResolvedValue({
      providerId: "codex",
      costHistory: [],
      creditsHistory: [],
      usageBreakdown: [],
      localUsage: null,
    });
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        PanelAllProviders: "All providers",
        PanelAllProvidersShort: "All",
        PanelLeftSuffix: "left",
        PanelShowAllProviders: "Show all providers",
        PanelShowFewerProviders: "Show fewer providers",
        PanelUsedSuffix: "used",
        SummaryProvidersLabel: "providers",
      }),
    );
    tauriMocks.openFlyoutWindow.mockResolvedValue(undefined);
    eventMocks.listen.mockImplementation(
      (eventName: string, handler: (event: unknown) => void) => {
        eventListeners.set(eventName, handler);
        return Promise.resolve(() => eventListeners.delete(eventName));
      },
    );
  });

  it("opens the requested provider as a first-class detail page", async () => {
    const { container } = renderPopOut(
      [provider("codex", "Codex", 80), provider("claude", "Claude", 30)],
      "claude",
    );

    await waitFor(() => {
      expect(container.querySelector(".provider-focus")).not.toBeNull();
    });
    expect(screen.getAllByText("Claude").length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText("Codex")).toBeNull();
    expect(screen.getByRole("button", { name: "Back to Overview" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Back to Overview" }));
    expect(container.querySelector(".provider-focus")).toBeNull();
    expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(2);
  });

  it("renders the dashboard chrome: rail, header, and status bar", async () => {
    const { container } = renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(container.querySelector(".dashboard")).not.toBeNull();
    });

    for (const name of ["Overview", "Activity", "Accounts", "Charts"]) {
      expect(screen.getByRole("button", { name })).toBeInTheDocument();
    }
    expect(
      screen.getByRole("button", { name: "Overview" }).getAttribute("aria-current"),
    ).toBe("page");
    expect(container.querySelector(".dashboard-header__title")?.textContent).toBe("Overview");
    expect(container.querySelector(".dashboard-header__sub")?.textContent).toBe(
      "Usage at a glance",
    );
    expect(container.querySelector(".dashboard-status")).not.toBeNull();
    expect(screen.getByText("All times local")).toBeInTheDocument();
  });

  it("switches dashboard sections from the rail", async () => {
    const { container } = renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(container.querySelector(".dashboard")).not.toBeNull();
    });

    // Activity is a real section now — switching to it shows the timeline, not
    // the foundation-phase placeholder.
    fireEvent.click(screen.getByRole("button", { name: "Activity" }));
    expect(container.querySelector(".dashboard-header__title")?.textContent).toBe("Activity");
    expect(
      screen.getByRole("button", { name: "Activity" }).getAttribute("aria-current"),
    ).toBe("page");
    expect(
      screen.getByRole("button", { name: "Overview" }).getAttribute("aria-current"),
    ).toBeNull();
    expect(container.querySelector(".activity-timeline")).not.toBeNull();
    expect(container.querySelector(".dashboard-placeholder")).toBeNull();

    // Accounts is a real section too — one card per provider, no placeholder.
    fireEvent.click(screen.getByRole("button", { name: "Accounts" }));
    expect(container.querySelector(".dashboard-header__title")?.textContent).toBe("Accounts");
    expect(container.querySelector(".accounts-panel")).not.toBeNull();
    expect(container.querySelector(".account-card")).not.toBeNull();
    expect(container.querySelector(".dashboard-placeholder")).toBeNull();
  });

  it("opens Settings from the rail", async () => {
    const { container } = renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(container.querySelector(".dashboard-rail")).not.toBeNull();
    });

    // The settings action is the last rail button (after the spacer).
    const railButtons = container.querySelectorAll<HTMLButtonElement>(".dashboard-rail__btn");
    fireEvent.click(railButtons[railButtons.length - 1]);

    expect(tauriMocks.openSettingsWindow).toHaveBeenCalledWith("general");
  });

  it("toggles the theme from the status bar", async () => {
    const { container } = renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(container.querySelector(".dashboard-status")).not.toBeNull();
    });

    // Default theme is dark, so the toggle switches to light.
    fireEvent.click(container.querySelector<HTMLButtonElement>(".dashboard-status__toggle")!);

    await waitFor(() =>
      expect(tauriMocks.updateSettings).toHaveBeenCalledWith({ theme: "light" }),
    );
  });

  it("renders cleanly with the flyout-window rewiring for goTray's onClick", async () => {
    // goTray's onClick now calls openFlyoutWindow() (formerly
    // setSurfaceMode("trayPanel", ...)) — asserted directly against the mock
    // import rather than via a click because `headerActions` (the array
    // goTray's handler lives in) is currently never rendered by
    // MenuSurface: `actions` is destructured in MenuSurfaceProps but not
    // consumed in its JSX (components/MenuSurface.tsx), so there is no
    // "back to tray" button in the DOM to click today. That's a pre-existing
    // gap tracked separately, not introduced by this rewiring. This test
    // instead pins down that the component still renders without error and
    // that openFlyoutWindow is never called on mount (only on the — for now
    // unreachable — click), so the rewiring doesn't regress anything that
    // currently DOES work.
    renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(screen.getAllByText("Codex").length).toBeGreaterThan(0);
    });

    expect(tauriMocks.openFlyoutWindow).not.toHaveBeenCalled();
  });

  it("applies the persisted PopOut display scale", async () => {
    const { container } = renderPopOut(
      [provider("codex", "Codex", 80)],
      undefined,
      [],
      { windowScalePercent: 175 },
    );

    await waitFor(() => {
      expect(container.querySelector(".popout-scale-shell")).not.toBeNull();
    });

    // Scaling is applied via the webview's native zoom, not an inline
    // `--window-scale` style (which the earlier CSS-zoom approach used).
    await waitFor(() => {
      expect(webviewWindowMocks.setZoom).toHaveBeenCalledWith(1.75);
    });
  });

  it("does not resize or reposition the native window on mount", async () => {
    renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(screen.getAllByText("Codex").length).toBeGreaterThan(0);
    });

    // The PopOut title bar reads window state (isMaximized) on mount, so
    // getCurrentWindow is legitimately called; assert only that the surface
    // itself never resizes or repositions the native window.
    expect(windowMocks.setSize).not.toHaveBeenCalled();
    expect(windowMocks.setPosition).not.toHaveBeenCalled();
  });

  it("localizes the settings rail action in Japanese", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({ TooltipSettings: "設定" }, "japanese"),
    );

    renderPopOut([provider("codex", "Codex", 80)]);

    // Settings now lives in the dashboard nav rail (an icon button with an
    // accessible label); About/Quit moved to the tray menu.
    expect(await screen.findByLabelText("設定")).toBeInTheDocument();
  });

  it("renders overview cards in settings catalog order instead of fetch order", async () => {
    const catalog: ProviderCatalogEntry[] = [
      { id: "codex", displayName: "Codex", cookieDomain: null },
      { id: "claude", displayName: "Claude", cookieDomain: null },
      { id: "cursor", displayName: "Cursor", cookieDomain: null },
    ];

    const { container } = renderPopOut(
      [
        provider("cursor", "Cursor", 15),
        provider("codex", "Codex", 95),
        provider("claude", "Claude", 40),
      ],
      undefined,
      catalog,
      { enabledProviders: ["codex", "claude", "cursor"] },
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(3);
    });

    expect(
      Array.from(container.querySelectorAll(".plan-status-card__name")).map(
        (node) => node.textContent,
      ),
    ).toEqual(["Codex", "Claude", "Cursor"]);
  });

  it("renders a card for every provider (no compact cap in the scrolling dashboard)", async () => {
    const providers = TEST_PROVIDER_CATALOG.map(([id, displayName], index) =>
      provider(id, displayName, (index * 7) % 100),
    );

    const { container } = renderPopOut(providers, undefined, [], {
      enabledProviders: providers.map((snapshot) => snapshot.providerId),
    });

    await waitFor(() => {
      expect(container.querySelector(".menu-stack")).not.toBeNull();
    });

    // The dashboard body scrolls, so every provider gets a card — no 4-card
    // compact cap or "show more" expander (both belonged to the old switcher).
    expect(container.querySelectorAll(".menu-stack__item").length).toBe(
      providers.length,
    );
    expect(container.querySelector(".provider-grid")).toBeNull();
  });

  it("removes a cached provider when settings stop tracking it", async () => {
    const initialSettings = {
      ...settings(),
      enabledProviders: ["codex", "gemini"],
    };
    tauriMocks.getSettingsSnapshot.mockResolvedValue(initialSettings);
    tauriMocks.getDetectedProviderAccounts.mockResolvedValue([
      {
        providerId: "gemini",
        displayName: "Gemini",
        status: "ready",
        sourceLabel: "Gemini CLI",
        detail: "Signed in and ready to track",
      },
    ]);

    const { container } = renderPopOut(
      [provider("codex", "Codex"), provider("gemini", "Gemini")],
      undefined,
      [],
      { enabledProviders: initialSettings.enabledProviders },
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(2);
    });

    tauriMocks.getSettingsSnapshot.mockResolvedValue({
      ...initialSettings,
      enabledProviders: ["codex"],
    });
    await act(async () => {
      eventListeners.get("settings-changed")?.({});
    });

    await waitFor(() => {
      expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(1);
    });
    expect(container.querySelector(".plan-status-card__name")?.textContent).toBe("Codex");
    expect(screen.queryByText("Gemini")).toBeNull();
    expect(screen.queryByText("Available to track")).toBeNull();
  });
});
