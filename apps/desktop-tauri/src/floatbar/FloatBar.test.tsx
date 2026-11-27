import { act, fireEvent, render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  getProviderChartData: vi.fn(),
  getProviderLocalUsageSummary: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
  getSettingsSnapshot: vi.fn(),
  getForegroundProvider: vi.fn(),
  updateSettings: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  openSettingsWindow: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
  listeners: new Map<string, Array<(event: { payload: unknown }) => void>>(),
}));

const windowMocks = vi.hoisted(() => ({
  getCurrentWindow: vi.fn(() => ({
    startDragging: vi.fn().mockResolvedValue(undefined),
  })),
}));

const coreMocks = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);
vi.mock("@tauri-apps/api/window", () => windowMocks);
vi.mock("@tauri-apps/api/core", () => coreMocks);

import FloatBar from "./FloatBar";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import type { BootstrapState, ProviderUsageSnapshot, SettingsSnapshot } from "../types/bridge";

function rateWindow(
  used: number,
  opts: {
    exhausted?: boolean;
    resetsAt?: string | null;
    resetDescription?: string | null;
  } = {},
) {
  return {
    usedPercent: used,
    remainingPercent: 100 - used,
    windowMinutes: null,
    resetsAt: opts.resetsAt ?? null,
    resetDescription: opts.resetDescription ?? null,
    isExhausted: opts.exhausted ?? false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function snapshot(
  id: string,
  display: string,
  used: number,
  opts: {
    exhausted?: boolean;
    error?: string | null;
    resetsAt?: string | null;
    resetDescription?: string | null;
  } = {},
): ProviderUsageSnapshot {
  return {
    providerId: id,
    displayName: display,
    primary: rateWindow(used, opts),
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "auto",
    updatedAt: "2026-05-15T00:00:00Z",
    error: opts.error ?? null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

function settings(overrides: Partial<SettingsSnapshot> = {}): SettingsSnapshot {
  return {
    enabledProviders: ["claude", "codex"],
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
    floatBarEnabled: true,
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
    floatBarContrast: "light-text",
    floatBarClickThrough: false,
    floatBarProviderIds: [],
    taskbarAccountByProvider: {},
    floatBarDarkText: false,
    floatBarShowResetInline: false,
    floatBarShowCost: false,
    ...overrides,
  };
}

function bootstrap(settingsOverrides: Partial<SettingsSnapshot> = {}): BootstrapState {
  return {
    contractVersion: "v1",
    providers: [],
    settings: settings(settingsOverrides),
  };
}

function renderFloatBar(state: BootstrapState) {
  return render(
    <LocaleProvider>
      <FloatBar state={state} />
    </LocaleProvider>,
  );
}

function emitFloatBarEvent(event: string, payload: unknown) {
  for (const listener of eventMocks.listeners.get(event) ?? []) {
    listener({ payload });
  }
}

describe("FloatBar", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventMocks.listeners.clear();
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    tauriMocks.getForegroundProvider.mockResolvedValue({
      providerId: null,
      lastActiveProviderId: null,
    });
    tauriMocks.getProviderLocalUsageSummary.mockResolvedValue(null);
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ResetsInHoursMinutes: "Resets in {}h {}m",
        ResetsInDaysHours: "Resets in {}d {}h",
        TrayResetsDueNow: "Resetting",
        PanelToday: "Today",
        PanelUsedSuffix: "used",
        FloatBarThirtyDayShort: "30d",
        FloatBarNoProviders: "No providers",
        FloatBarRemainingSuffix: "remaining",
      }),
    );
    eventMocks.listen.mockImplementation(
      (event: string, handler: (event: { payload: unknown }) => void) => {
        const listeners = eventMocks.listeners.get(event) ?? [];
        listeners.push(handler);
        eventMocks.listeners.set(event, listeners);
        return Promise.resolve(() => {});
      },
    );
  });

  it("renders a pill per enabled provider, sorted by usage descending", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20),
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarShowCost: true }),
    );

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      const pills = container.querySelectorAll(".floatbar__pill");
      expect(pills.length).toBe(2);
    });

    const titles = Array.from(container.querySelectorAll(".floatbar__pill")).map(
      (el) => el.getAttribute("title") ?? "",
    );
    // Highest used (codex, 75%) shows first; display follows showAsUsed.
    expect(titles[0]).toMatch(/Codex: 75% used/);
    expect(titles[1]).toMatch(/Claude: 20% used/);
  });

  it("does not prefix a localized freshness chip with English state", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({ FreshnessStale: "数据过期" }),
    );
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 40),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill")).toBeInTheDocument();
    });
    const title = container.querySelector(".floatbar__pill")?.getAttribute("title") ?? "";
    expect(title).toContain("数据过期");
    expect(title).not.toContain("state ");
  });

  it("calm mode leads with a pace state and expands to the exact % on click", async () => {
    const calmProvider: ProviderUsageSnapshot = {
      ...snapshot("codex", "Codex", 60, {
        resetsAt: new Date(Date.now() + 2 * 3600_000).toISOString(),
      }),
      updatedAt: new Date().toISOString(),
      pace: {
        windowLabel: "Weekly",
        stage: "on_track",
        deltaPercent: 0,
        willLastToReset: true,
        etaSeconds: null,
        expectedUsedPercent: 60,
        actualUsedPercent: 60,
      },
    };
    tauriMocks.getCachedProviders.mockResolvedValue([calmProvider]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarInformationMode: "calm" }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarInformationMode: "calm" }),
    );
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--calm")).toBeInTheDocument();
    });

    // Calm leads with the trustworthy pace state; the exact % is hidden until
    // the user expands, and the pill is a keyboard-accessible button.
    expect(container.querySelector(".floatbar__pace")).toHaveTextContent("On pace");
    expect(container.querySelector(".floatbar__pct--calm")).toBeNull();
    const pill = container.querySelector<HTMLElement>(".floatbar__pill--calm")!;
    expect(pill.getAttribute("role")).toBe("button");
    expect(pill.getAttribute("tabindex")).toBe("0");

    act(() => {
      pill.click();
    });
    expect(container.querySelector(".floatbar__pct--calm")).toHaveTextContent("60%");
  });

  it("calm + compact stays non-blank and drops the window·reset row", async () => {
    // No fresh pace + compact density (which hides the window/reset): the pill
    // must still show the exact % rather than collapse to just the icon, and
    // the calm reset row (with its separator) must not render.
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 60, {
        resetsAt: new Date(Date.now() + 3600_000).toISOString(),
      }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarInformationMode: "calm", floatBarDensity: "compact" }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarInformationMode: "calm", floatBarDensity: "compact" }),
    );
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--calm")).toBeInTheDocument();
    });

    expect(container.querySelector(".floatbar__calm-reset")).toBeNull();
    expect(container.querySelector(".floatbar__calm-sep")).toBeNull();
    expect(container.querySelector(".floatbar__pct--calm")).toHaveTextContent("60%");
  });

  /**
   * SBS-876: exact mode already dropped the pace and reset for a placeholder
   * window. Calm still built both from the required 0% primary, so the pill
   * read "On pace · resets in 5d" for a plan with no reading at all.
   */
  it("calm mode names the state instead of pacing an unavailable plan", async () => {
    const unavailable: ProviderUsageSnapshot = {
      ...snapshot("cursor", "Cursor", 0, {
        resetsAt: new Date(Date.now() + 5 * 24 * 3600_000).toISOString(),
      }),
      updatedAt: new Date().toISOString(),
      inactiveRateWindows: [
        {
          id: "cursor-plan",
          title: "Plan",
          description: "No usage reported",
          state: "unavailable",
        },
      ],
      pace: {
        windowLabel: "Plan",
        stage: "far_ahead",
        deltaPercent: -38.6,
        willLastToReset: true,
        etaSeconds: null,
        expectedUsedPercent: 38.6,
        actualUsedPercent: 0,
      },
    };
    tauriMocks.getCachedProviders.mockResolvedValue([unavailable]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarInformationMode: "calm", enabledProviders: ["cursor"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarInformationMode: "calm", enabledProviders: ["cursor"] }),
    );
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--calm")).toBeInTheDocument();
    });

    expect(container.querySelector(".floatbar__pct--calm")).toHaveTextContent(
      "Unavailable",
    );
    expect(container.querySelector(".floatbar__pace")).toBeNull();
    expect(container.querySelector(".floatbar__calm-reset")).toBeNull();
    expect(container.querySelector(".floatbar__pill--calm")?.getAttribute("title"))
      .not.toContain("0%");
  });

  it("does not render hypothetical local costs from the legacy setting", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap({ floatBarShowCost: true }));

    await waitFor(() => {
      expect(tauriMocks.getCachedProviders).toHaveBeenCalled();
    });
    expect(container.querySelector(".floatbar__cost-pill")).not.toBeInTheDocument();
    expect(tauriMocks.getProviderLocalUsageSummary).not.toHaveBeenCalled();
    expect(tauriMocks.getProviderChartData).not.toHaveBeenCalled();
  });

  it("does not scan local costs by default", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    renderFloatBar(bootstrap());

    await waitFor(() => {
      expect(tauriMocks.getCachedProviders).toHaveBeenCalled();
    });
    expect(tauriMocks.getProviderLocalUsageSummary).not.toHaveBeenCalled();
  });

  it("can show remaining percentages when configured", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings({ showAsUsed: false }));

    const { container } = renderFloatBar(bootstrap({ showAsUsed: false }));

    await waitFor(() => {
      const title = container
        .querySelector(".floatbar__pill")
        ?.getAttribute("title");
      expect(title).toContain("Claude: 80% remaining");
    });
  });

  it("headlines Cursor Auto/API remaining capacity, not Plan total", async () => {
    const live = snapshot("cursor", "Cursor", 20);
    live.updatedAt = new Date().toISOString();
    live.primaryLabel = "Monthly";
    live.secondary = rateWindow(70);
    live.secondaryLabel = "Auto";
    live.extraRateWindows = [
      {
        id: "cursor-api",
        title: "API",
        window: rateWindow(100),
      },
    ];
    tauriMocks.getCachedProviders.mockResolvedValue([live]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({
        showAsUsed: true,
        enabledProviders: ["cursor"],
      }),
    );

    const { container } = renderFloatBar(
      bootstrap({ showAsUsed: true, enabledProviders: ["cursor"] }),
    );

    await waitFor(() => {
      // API is maxed; Auto still has room → strip shows Auto, not Plan 20%.
      expect(container.querySelector(".floatbar__window")?.textContent).toBe(
        "Auto",
      );
      expect(container.querySelector(".floatbar__pct")?.textContent).toBe("70%");
      expect(container.querySelector(".floatbar__pill--crit")).toBeNull();
      // No tiny companion chip anymore.
      expect(container.querySelector(".floatbar__companion")).toBeNull();
      // Fresh timestamp → live pill, no state chip.
      expect(container.querySelector(".floatbar__chip")).toBeNull();
      expect(container.querySelector(".floatbar__pill--lifted")).toBeNull();
    });
  });

  it("headlines on-demand spend in dollars once every allowance is depleted", async () => {
    // SBS-191: the free-floating bar had the same defect as the native tile —
    // it picked the on-demand lane correctly, then rendered "62%".
    const live = snapshot("cursor", "Cursor", 100);
    live.updatedAt = new Date().toISOString();
    live.primaryLabel = "Plan";
    live.secondary = rateWindow(100);
    live.secondaryLabel = "Auto";
    live.extraRateWindows = [
      { id: "cursor-api", title: "API", window: rateWindow(100) },
      {
        id: "cursor-on-demand",
        title: "On-demand",
        window: rateWindow(62),
        amount: {
          used: 1112.92,
          limit: 1800,
          currencyCode: "USD",
          formattedUsed: "$1112.92",
          formattedLimit: "$1800.00",
        },
      },
    ];
    tauriMocks.getCachedProviders.mockResolvedValue([live]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ showAsUsed: true, enabledProviders: ["cursor"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ showAsUsed: true, enabledProviders: ["cursor"] }),
    );

    await waitFor(() => {
      expect(container.querySelector(".floatbar__window")?.textContent).toBe(
        "On-demand",
      );
      expect(container.querySelector(".floatbar__pct")?.textContent).toBe(
        "$1112.92",
      );
    });
  });

  it("animates only the provider named by a confirmed capacity event", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 40),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2));

    act(() => {
      emitFloatBarEvent("capacity-event", {
        providerId: "codex",
        displayName: "Codex",
        windowId: "session",
        windowLabel: "Session",
        kind: "surpriseReset",
        previousUsedPercent: 92,
        currentUsedPercent: 4,
        previousResetAt: "2026-07-13T20:00:00Z",
        currentResetAt: "2026-07-14T01:00:00Z",
        occurredAt: "2026-07-13T16:00:00Z",
      });
    });

    const pills = Array.from(container.querySelectorAll(".floatbar__pill"));
    const codexPill = pills.find((pill) => pill.getAttribute("title")?.includes("Codex"));
    const claudePill = pills.find((pill) => pill.getAttribute("title")?.includes("Claude"));
    expect(codexPill).toHaveClass("floatbar__pill--surpriseReset");
    expect(claudePill).not.toHaveClass("floatbar__pill--capacity-event");
  });

  it("applies warning tone when remaining drops below the high threshold", async () => {
    // highUsageThreshold = 70 → high-remaining cutoff = 30%.
    // claude at 80% used → 20% remaining → critical (also below crit cutoff 10).
    // Use 75% used → 25% remaining → warn (between 10 and 30).
    tauriMocks.getCachedProviders.mockResolvedValue([snapshot("claude", "Claude", 75)]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--warn")).not.toBeNull();
    });
  });

  it("applies critical tone when the provider is exhausted", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 100, { exhausted: true }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--crit")).not.toBeNull();
    });
  });

  it("active mode shows only the last focused provider", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 50),
    ]);
    tauriMocks.getForegroundProvider.mockResolvedValue({
      providerId: "claude",
      lastActiveProviderId: "claude",
    });
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarSelectionMode: "active" }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarSelectionMode: "active" }),
    );
    await waitFor(() => {
      const pills = container.querySelectorAll(".floatbar__pill");
      expect(pills.length).toBe(1);
      expect(pills[0].getAttribute("title")).toMatch(/Claude/);
    });
  });

  it("filters to the floatBarProviderIds allowlist when non-empty", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 50),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarProviderIds: ["codex"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarProviderIds: ["codex"] }),
    );
    await waitFor(() => {
      const pills = container.querySelectorAll(".floatbar__pill");
      expect(pills.length).toBe(1);
      expect(pills[0].getAttribute("title")).toMatch(/Codex/);
    });
  });

  it("keeps floatBarProviderIds order instead of sorting by usage", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 90),
      snapshot("codex", "Codex", 10),
      snapshot("grok", "Grok", 50),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({
        enabledProviders: ["claude", "codex", "grok"],
        floatBarProviderIds: ["grok", "codex", "claude"],
      }),
    );

    const { container } = renderFloatBar(
      bootstrap({
        enabledProviders: ["claude", "codex", "grok"],
        floatBarProviderIds: ["grok", "codex", "claude"],
      }),
    );
    await waitFor(() => {
      const titles = [...container.querySelectorAll(".floatbar__pill")].map((el) =>
        el.getAttribute("title") ?? "",
      );
      expect(titles[0]).toMatch(/Grok/);
      expect(titles[1]).toMatch(/Codex/);
      expect(titles[2]).toMatch(/Claude/);
    });
  });

  it("does not show stale cached providers when all providers are disabled", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 50),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: [] }),
    );

    const { container } = renderFloatBar(bootstrap({ enabledProviders: [] }));
    await waitFor(() => {
      expect(container.querySelectorAll(".floatbar__pill").length).toBe(0);
      expect(container.querySelector(".floatbar__empty")).not.toBeNull();
    });
  });

  it("hides enabled providers that have never successfully authenticated", async () => {
    // Repro: enable Antigravity without the IDE/CLI installed. The refresh
    // lands an error placeholder whose session_label is "Claude", which used
    // to mint a blank "—" / Claude pill on the strip.
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 36),
      {
        ...snapshot("antigravity", "Antigravity", 0, {
          error:
            "Provider not installed: Antigravity CLI (`agy`) is not installed. Install Antigravity CLI 1.2.13 or later, sign in, then retry.",
        }),
        primaryLabel: "Claude",
      },
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: ["codex", "antigravity"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ enabledProviders: ["codex", "antigravity"] }),
    );

    await waitFor(() => {
      const pills = [...container.querySelectorAll(".floatbar__pill")];
      expect(pills).toHaveLength(1);
      expect(pills[0].getAttribute("title")).toMatch(/Codex/);
      expect(pills.some((p) => (p.getAttribute("title") ?? "").includes("Claude"))).toBe(
        false,
      );
      expect(
        pills.some((p) => (p.getAttribute("title") ?? "").includes("Antigravity")),
      ).toBe(false);
    });
  });

  it("shows an empty state when no providers match", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__empty")).not.toBeNull();
    });
  });

  it("applies the light-background class and CSS opacity", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarContrast: "dark-text", floatBarOpacity: 45 }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarContrast: "dark-text", floatBarOpacity: 45 }),
    );

    await waitFor(() => {
      const bar = container.querySelector<HTMLElement>(".floatbar");
      expect(bar).not.toBeNull();
      expect(bar?.classList.contains("floatbar--light-bg")).toBe(true);
      expect(bar?.style.opacity).toBe("0.45");
    });
  });

  it("applies the configured scale as a CSS variable", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings({ floatBarScale: 150 }));

    const { container } = renderFloatBar(bootstrap({ floatBarScale: 150 }));

    await waitFor(() => {
      const bar = container.querySelector<HTMLElement>(".floatbar");
      expect(bar).not.toBeNull();
      expect(bar?.style.getPropertyValue("--floatbar-scale")).toBe("1.5");
    });
  });

  it("uses the localized reset formatter in pill tooltips", async () => {
    const resetsAt = new Date(Date.now() + 3 * 60 * 60_000 + 42 * 60_000).toISOString();
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20, { resetsAt }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());

    await waitFor(() => {
      const title = container
        .querySelector(".floatbar__pill")
        ?.getAttribute("title");
      expect(title).toContain("Claude: 20% used");
      expect(title).toMatch(/Resets in 3h 4[12]m/);
      expect(title).not.toContain("Resets in due now");
    });
  });

  it("can render a next reset icon and time in provider pills", async () => {
    const resetsAt = new Date(Date.now() + 2 * 60 * 60_000 + 5 * 60_000).toISOString();
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20, { resetsAt }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarShowResetInline: true }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarShowResetInline: true }),
    );

    await waitFor(() => {
      const reset = container.querySelector(".floatbar__reset");
      expect(reset).not.toBeNull();
      expect(reset?.getAttribute("aria-label")).toMatch(/Resets in 2h [45]m/);
      expect(reset?.textContent).toMatch(/2h [45]m/);
      expect(reset?.textContent).not.toContain("Resets in");
    });
  });

  it("shows reset on depleted pills even when inline reset is disabled", async () => {
    const resetsAt = new Date(Date.now() + 4 * 60 * 60_000).toISOString();
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 100, { exhausted: true, resetsAt }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarShowResetInline: false }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarShowResetInline: false }),
    );

    await waitFor(() => {
      const reset = container.querySelector(".floatbar__reset--emphasis");
      expect(reset).not.toBeNull();
      expect(reset?.textContent).toMatch(/[34]h/);
      expect(container.querySelector(".floatbar__chip--promo")).toBeNull();
    });
  });

  it("keeps promo signals out of the strip chrome", async () => {
    const live = snapshot("cursor", "Cursor", 84);
    live.updatedAt = new Date().toISOString();
    live.primaryLabel = "Plan";
    live.promoSignals = [
      {
        id: "promo-1",
        kind: "boost",
        title: "promotional",
        description: "Extra capacity",
      },
    ];
    tauriMocks.getCachedProviders.mockResolvedValue([live]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: ["cursor"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ enabledProviders: ["cursor"] }),
    );

    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill")).not.toBeNull();
      expect(container.querySelector(".floatbar__chip--promo")).toBeNull();
      expect(container.querySelector(".floatbar__pill--promo-boost")).toBeNull();
    });

    // Out of the chrome, but not lost: the tooltip is the whole of the strip's
    // promo contract, and asserting only the absent classes left that half
    // unpinned — dropping the title from `titleBits` would still have passed.
    expect(container.querySelector(".floatbar__pill")?.getAttribute("title")).toContain(
      "promo promotional",
    );
  });

  it("opens the live Display settings tab from the menu, not menuBar (SBS-872)", async () => {
    // Failure mode: FloatBar sent menuBar, Settings.isSettingsTab rejected
    // it, and the detached window landed on General. PopOutPanel already
    // uses the live menu id for the same Display tab.
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 40),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
    tauriMocks.openSettingsWindow.mockResolvedValue(undefined);

    const { container, getByText } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar")).not.toBeNull();
    });

    fireEvent.contextMenu(container.querySelector(".floatbar")!);
    fireEvent.click(getByText("FloatBarOpenSettings"));

    expect(tauriMocks.openSettingsWindow).toHaveBeenCalledWith("menu");
    expect(tauriMocks.openSettingsWindow).not.toHaveBeenCalledWith("menuBar");
  });

  it("polls refreshProvidersIfStale on the configured interval", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([]);
      tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
      await act(async () => {
        renderFloatBar(bootstrap({ refreshIntervalSecs: 120 }));
      });

      // Initial tick fires synchronously on mount; useProviders is passive here
      // so the floatbar does not double-request stale refreshes at startup.
      await vi.waitFor(() => {
        expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
      });
      const initialCalls = tauriMocks.refreshProvidersIfStale.mock.calls.length;

      await vi.advanceTimersByTimeAsync(119_000);
      expect(tauriMocks.refreshProvidersIfStale.mock.calls.length).toBe(initialCalls);

      await vi.advanceTimersByTimeAsync(1_000);
      expect(tauriMocks.refreshProvidersIfStale.mock.calls.length).toBeGreaterThan(
        initialCalls,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not poll when refresh_interval_secs is 0 (manual only)", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([]);
      tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
      await act(async () => {
        renderFloatBar(bootstrap({ refreshIntervalSecs: 0 }));
      });

      expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(300_000);
      expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
