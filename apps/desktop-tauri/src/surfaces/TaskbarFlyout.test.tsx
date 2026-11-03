import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { BootstrapState, ProviderUsageSnapshot } from "../types/bridge";

const windowMocks = vi.hoisted(() => ({
  setSize: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => windowMocks,
  LogicalSize: class LogicalSize {
    constructor(public width: number, public height: number) {}
  },
}));

const tauriMocks = vi.hoisted(() => ({
  dismissTrayPanel: vi.fn().mockResolvedValue(undefined),
  getTaskbarSurfaceColor: vi.fn().mockResolvedValue("#073b78"),
  reanchorTrayPanel: vi.fn().mockResolvedValue(undefined),
  revealTrayPanelWindow: vi.fn().mockResolvedValue(undefined),
  setSurfaceMode: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/tauri", () => tauriMocks);

const providerState = vi.hoisted(() => ({
  providers: [] as ProviderUsageSnapshot[],
}));

vi.mock("../hooks/useProviders", () => ({
  useProviders: () => ({
    providers: providerState.providers,
    hasLoadedCache: true,
  }),
}));

vi.mock("../hooks/useSettings", () => ({
  useSettings: (settings: unknown) => ({ settings }),
}));

vi.mock("../hooks/useLocale", () => ({
  useLocale: () => ({
    t: (key: string) => {
      if (key === "NotCurrentlyEnforced") return "Not currently enforced";
      if (key === "WindowUnavailable") return "Unavailable";
      return key;
    },
  }),
}));

import TaskbarFlyout from "./TaskbarFlyout";

function provider(
  providerId: string,
  displayName: string,
  usedPercent: number,
  resetMinutes: number,
  primaryLabel: string,
): ProviderUsageSnapshot {
  return {
    providerId,
    displayName,
    primary: {
      usedPercent,
      remainingPercent: 100 - usedPercent,
      windowMinutes: 300,
      resetsAt: new Date(Date.now() + resetMinutes * 60_000).toISOString(),
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    primaryLabel,
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "test",
    updatedAt: new Date().toISOString(),
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

const state = {
  providers: [
    { id: "codex", displayName: "Codex" },
    { id: "claude", displayName: "Claude" },
  ],
  settings: {
    enabledProviders: ["codex", "claude"],
    providerOrder: ["codex", "claude"],
    showAsUsed: true,
  },
} as BootstrapState;

describe("TaskbarFlyout", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    providerState.providers = [
      provider("codex", "Codex", 41, 6 * 24 * 60, "Weekly"),
      provider("claude", "Claude", 25, 212, "Session (5h)"),
    ];
  });

  it("shows both accounts' emails when a provider has two (screenshot-1 bug)", async () => {
    // Exactly the reported setup: two Codex accounts in the taskbar flyout.
    const personal = provider("codex", "Codex", 46, 6 * 24 * 60, "Weekly");
    personal.accountId = "acct-personal";
    personal.accountEmail = "tsouth2@gmail.com";
    personal.planName = "Pro Lite";
    const work = provider("codex", "Codex", 16, 6 * 24 * 60, "Weekly");
    work.accountId = "acct-work";
    work.accountEmail = "bts@cssi.us";
    work.planName = "ChatGPT Team";
    providerState.providers = [personal, work];

    render(<TaskbarFlyout state={state} />);

    // Both accounts appear, each named by its own email — not two bare "Codex"
    // rows, which is what the user saw.
    expect(
      await screen.findByText("tsouth2@gmail.com (Pro Lite)"),
    ).toBeInTheDocument();
    expect(screen.getByText("bts@cssi.us (ChatGPT Team)")).toBeInTheDocument();
    // Both rows still render.
    expect(screen.getAllByText("Codex")).toHaveLength(2);
    expect(screen.getByText("46%")).toBeInTheDocument();
    expect(screen.getByText("16%")).toBeInTheDocument();
  });

  it("marks the strip account and lists it first when multi-account", async () => {
    const personal = provider("codex", "Codex", 20, 6 * 24 * 60, "Weekly");
    personal.accountId = "acct-personal";
    personal.accountEmail = "me@home.test";
    const work = provider("codex", "Codex", 80, 6 * 24 * 60, "Weekly");
    work.accountId = "acct-work";
    work.accountEmail = "me@job.test";
    providerState.providers = [work, personal];
    const pinnedState = {
      ...state,
      settings: {
        ...state.settings,
        floatBarProviderIds: ["codex"],
        taskbarAccountByProvider: { codex: "acct-personal" },
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={pinnedState} />);

    expect(await screen.findByText("On strip")).toBeInTheDocument();
    const accounts = screen.getAllByText(/me@/);
    // Pinned personal seat leads even though work is hotter.
    expect(accounts[0].textContent).toContain("me@home.test");
    expect(accounts[1].textContent).toContain("me@job.test");
  });

  it("badges the seat the strip actually shows, not the hottest session", async () => {
    // Reported bug: on "auto (closest to limit)" the tile showed the maxed
    // weekly seat while the flyout badged the other one, because the flyout
    // ranked accounts on the 5h session and both read 0%.
    const maxedWeekly = provider("claude", "Claude", 0, 300, "Session (5h)");
    maxedWeekly.accountId = "acct-work";
    maxedWeekly.accountEmail = "me@job.test";
    maxedWeekly.secondary = {
      ...maxedWeekly.primary,
      usedPercent: 100,
      remainingPercent: 0,
      windowMinutes: 10_080,
    };
    maxedWeekly.secondaryLabel = "Weekly";
    const fresh = provider("claude", "Claude", 0, 295, "Session (5h)");
    fresh.accountId = "acct-zpersonal";
    fresh.accountEmail = "me@home.test";
    fresh.secondary = {
      ...fresh.primary,
      usedPercent: 0,
      remainingPercent: 100,
      windowMinutes: 10_080,
    };
    fresh.secondaryLabel = "Weekly";
    providerState.providers = [maxedWeekly, fresh];
    const autoState = {
      ...state,
      settings: {
        ...state.settings,
        floatBarProviderIds: ["claude"],
        taskbarAccountByProvider: {},
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={autoState} />);

    await screen.findByText("On strip");
    const accounts = screen.getAllByText(/me@/);
    expect(accounts[0].textContent).toContain("me@job.test");
    // The badge sits in the same row as the maxed-weekly seat.
    const badgedRow = screen.getByText("On strip").closest(".taskbar-flyout__provider");
    expect(badgedRow?.textContent).toContain("me@job.test");
    expect(badgedRow?.textContent).not.toContain("me@home.test");
  });

  it("shows at-a-glance usage and the soonest provider reset", async () => {
    providerState.providers[0].resetCreditsAvailable = 1;
    render(<TaskbarFlyout state={state} />);

    expect(screen.getByText("Ceiling")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.getByText("Claude")).toBeInTheDocument();
    expect(screen.getByText("41%")).toBeInTheDocument();
    expect(screen.getByText("25%")).toBeInTheDocument();
    expect(screen.getByText(/1 reset ready/)).toBeInTheDocument();
    expect(screen.getByText(/^Next reset in 3h/)).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Claude Session (5h) 25%" })).toHaveAttribute("data-level", "normal");

    await waitFor(() => {
      expect(windowMocks.setSize).toHaveBeenCalled();
      expect(tauriMocks.reanchorTrayPanel).toHaveBeenCalled();
      expect(tauriMocks.revealTrayPanelWindow).toHaveBeenCalled();
    });
  });

  it("shows each useful allowance without promotional Cursor pools", async () => {
    const claude = provider("claude", "Claude", 9, 240, "Session (5h)");
    claude.secondary = {
      ...claude.primary,
      usedPercent: 22,
      remainingPercent: 78,
      resetsAt: new Date(Date.now() + 5 * 24 * 60 * 60_000).toISOString(),
    };
    claude.secondaryLabel = "Weekly";
    const cursor = provider("cursor", "Cursor", 85, 22 * 24 * 60, "Plan");
    cursor.secondary = { ...cursor.primary, usedPercent: 99, remainingPercent: 1 };
    cursor.secondaryLabel = "Auto";
    cursor.extraRateWindows = [
      {
        id: "cursor-api",
        title: "API",
        window: { ...cursor.primary, usedPercent: 38, remainingPercent: 62 },
      },
      {
        id: "cursor-promotional",
        title: "Promotional",
        window: { ...cursor.primary, usedPercent: 100, remainingPercent: 0 },
      },
    ];
    providerState.providers = [claude, cursor];
    const multiWindowState = {
      ...state,
      providers: [
        { id: "claude", displayName: "Claude" },
        { id: "cursor", displayName: "Cursor" },
      ],
      settings: {
        ...state.settings,
        enabledProviders: ["claude", "cursor"],
        providerOrder: ["claude", "cursor"],
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={multiWindowState} />);

    expect(screen.getByRole("progressbar", { name: "Claude Session (5h) 9%" })).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Claude Weekly 22%" })).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Cursor Plan 85%" })).toHaveAttribute("data-level", "warning");
    expect(screen.getByRole("progressbar", { name: "Cursor Auto 99%" })).toHaveAttribute("data-level", "critical");
    expect(screen.getByRole("progressbar", { name: "Cursor API 38%" })).toHaveAttribute("data-level", "normal");
    expect(screen.queryByText("Promotional")).not.toBeInTheDocument();
  });

  it("adds Cursor's Grok Bot row without displacing On-demand", () => {
    const cursor = provider("cursor", "Cursor", 85, 22 * 24 * 60, "Plan");
    cursor.secondary = { ...cursor.primary, usedPercent: 40, remainingPercent: 60 };
    cursor.secondaryLabel = "Auto";
    cursor.extraRateWindows = [
      {
        id: "cursor-api",
        title: "API",
        window: { ...cursor.primary, usedPercent: 38, remainingPercent: 62 },
      },
      {
        id: "cursor-grok-bot",
        title: "Grok Bot",
        window: { ...cursor.primary, usedPercent: 43, remainingPercent: 57 },
      },
      {
        id: "cursor-on-demand",
        title: "On-demand",
        window: { ...cursor.primary, usedPercent: 12, remainingPercent: 88 },
      },
    ];
    providerState.providers = [cursor];
    const cursorState = {
      ...state,
      providers: [{ id: "cursor", displayName: "Cursor" }],
      settings: {
        ...state.settings,
        enabledProviders: ["cursor"],
        providerOrder: ["cursor"],
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={cursorState} />);

    expect(screen.getByRole("progressbar", { name: "Cursor Grok Bot 43%" })).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Cursor On-demand 12%" })).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Cursor Plan 85%" })).toBeInTheDocument();
  });

  it("keeps the On-demand lane and its spend when every Cursor allowance is depleted", async () => {
    // SBS-191: the reporter's exact state — Plan/Auto/API all at 100%, real
    // money accruing on-demand. The flyout used to drop the row by name, so the
    // one lane still costing anything was the only one you could not see.
    const cursor = provider("cursor", "Cursor", 100, 22 * 24 * 60, "Plan");
    cursor.primary.isExhausted = true;
    cursor.secondary = {
      ...cursor.primary,
      usedPercent: 100,
      remainingPercent: 0,
      isExhausted: true,
    };
    cursor.secondaryLabel = "Auto";
    cursor.extraRateWindows = [
      {
        id: "cursor-api",
        title: "API",
        window: {
          ...cursor.primary,
          usedPercent: 100,
          remainingPercent: 0,
          isExhausted: true,
        },
      },
      {
        id: "cursor-on-demand",
        title: "On-demand",
        window: { ...cursor.primary, usedPercent: 62, remainingPercent: 38, isExhausted: false },
        amount: {
          used: 1112.92,
          limit: 1800,
          currencyCode: "USD",
          formattedUsed: "$1112.92",
          formattedLimit: "$1800.00",
        },
      },
    ];
    providerState.providers = [cursor];
    const cursorState = {
      ...state,
      providers: [{ id: "cursor", displayName: "Cursor" }],
      settings: {
        ...state.settings,
        enabledProviders: ["cursor"],
        providerOrder: ["cursor"],
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={cursorState} />);

    expect(screen.getByText("On-demand")).toBeInTheDocument();
    expect(screen.getByText("$1112.92 of $1800.00")).toBeInTheDocument();
    expect(
      screen.getByRole("progressbar", {
        name: "Cursor On-demand 62% — $1112.92 of $1800.00",
      }),
    ).toBeInTheDocument();
    // All four lanes fit, so nothing is silently truncated.
    expect(screen.queryByText(/more limits in Ceiling/)).not.toBeInTheDocument();
  });

  it("states spend of limit under Show-as-remaining, like the Overview does", () => {
    // Rows with space print both numbers, so the used/remaining toggle has
    // nothing left to reveal and moves only the percentage — the same split
    // PlanStatusCard uses. Only the one-number strips (taskbar tile, floating
    // bar) pick one figure and follow the setting.
    const cursor = provider("cursor", "Cursor", 100, 22 * 24 * 60, "Plan");
    cursor.primary.isExhausted = true;
    cursor.extraRateWindows = [
      {
        id: "cursor-on-demand",
        title: "On-demand",
        window: {
          ...cursor.primary,
          usedPercent: 62,
          remainingPercent: 38,
          isExhausted: false,
        },
        amount: {
          used: 1112.92,
          limit: 1800,
          currencyCode: "USD",
          formattedUsed: "$1112.92",
          formattedLimit: "$1800.00",
        },
      },
    ];
    providerState.providers = [cursor];

    render(
      <TaskbarFlyout
        state={
          {
            ...state,
            providers: [{ id: "cursor", displayName: "Cursor" }],
            settings: {
              ...state.settings,
              enabledProviders: ["cursor"],
              providerOrder: ["cursor"],
              showAsUsed: false,
            },
          } as BootstrapState
        }
      />,
    );

    // The percentage flips to remaining...
    expect(screen.getByText("38%")).toBeInTheDocument();
    // ...while the money keeps stating both figures.
    expect(screen.getByText("$1112.92 of $1800.00")).toBeInTheDocument();
  });

  /**
   * SBS-876: a missing Cursor plan used to render a lone Plan 0% bar because
   * flyoutWindows preferred primary and ignored inactiveRateWindows.
   */
  it("does not render a 0% Plan bar when Cursor monthly is unavailable", () => {
    const cursor = provider("cursor", "Cursor", 0, 22 * 24 * 60, "Plan");
    cursor.inactiveRateWindows = [
      {
        id: "cursor-plan",
        title: "Plan",
        description: "No usage reported",
        state: "unavailable",
      },
    ];
    providerState.providers = [cursor];

    render(
      <TaskbarFlyout
        state={
          {
            ...state,
            providers: [{ id: "cursor", displayName: "Cursor" }],
            settings: {
              ...state.settings,
              enabledProviders: ["cursor"],
              providerOrder: ["cursor"],
            },
          } as BootstrapState
        }
      />,
    );

    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    expect(screen.getByText("Plan")).toBeInTheDocument();
    expect(
      screen.queryByRole("progressbar", { name: /Cursor Plan 0%/ }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("0%")).not.toBeInTheDocument();
  });

  /**
   * SBS-876 follow-up: dropping `primary` from the Cursor preference list to
   * make room for the named-state row also demoted a *real* Plan reading to the
   * leftover lane, so it rendered last — under On-demand — instead of leading
   * the provider it is the headline allowance for.
   */
  it("keeps a real Cursor Plan at the top of its lanes", () => {
    const cursor = provider("cursor", "Cursor", 51, 22 * 24 * 60, "Plan");
    cursor.secondary = { ...cursor.primary, usedPercent: 99, remainingPercent: 1 };
    cursor.secondaryLabel = "Auto";
    cursor.extraRateWindows = [
      {
        id: "cursor-api",
        title: "API",
        window: { ...cursor.primary, usedPercent: 38, remainingPercent: 62 },
      },
      {
        id: "cursor-on-demand",
        title: "On-demand",
        window: { ...cursor.primary, usedPercent: 62, remainingPercent: 38 },
      },
    ];
    providerState.providers = [cursor];

    const { container } = render(
      <TaskbarFlyout
        state={
          {
            ...state,
            providers: [{ id: "cursor", displayName: "Cursor" }],
            settings: {
              ...state.settings,
              enabledProviders: ["cursor"],
              providerOrder: ["cursor"],
            },
          } as BootstrapState
        }
      />,
    );

    expect(
      screen.getByRole("progressbar", { name: "Cursor Plan 51%" }),
    ).toBeInTheDocument();
    const labels = Array.from(
      container.querySelectorAll(".taskbar-flyout__meter-label"),
    ).map((node) => node.textContent);
    expect(labels).toEqual(["Plan", "Auto", "API", "On-demand"]);
    expect(screen.queryByText(/more limits in Ceiling/)).not.toBeInTheDocument();
  });

  /**
   * SBS-876: named-state rows compete for the same four visible slots, but the
   * hidden-row count only measured metered windows. Three measured rows plus
   * two Unavailable rows rendered four and reported nothing hidden, because the
   * subtraction went negative and was clamped to zero.
   */
  it("counts truncated named-state rows in the more-limits note", () => {
    const claude = provider("claude", "Claude", 40, 300, "Session (5h)");
    claude.secondary = { ...claude.primary, usedPercent: 61, remainingPercent: 39 };
    claude.secondaryLabel = "Weekly";
    claude.extraRateWindows = [
      {
        id: "claude-opus",
        title: "Opus weekly",
        window: { ...claude.primary, usedPercent: 12, remainingPercent: 88 },
      },
    ];
    claude.inactiveRateWindows = [
      { id: "one", title: "Sonnet weekly", description: "", state: "unavailable" },
      { id: "two", title: "Haiku weekly", description: "", state: "unavailable" },
    ];
    providerState.providers = [claude];

    render(
      <TaskbarFlyout
        state={
          {
            ...state,
            providers: [{ id: "claude", displayName: "Claude" }],
            settings: {
              ...state.settings,
              enabledProviders: ["claude"],
              providerOrder: ["claude"],
            },
          } as BootstrapState
        }
      />,
    );

    // 3 measured + 2 named = 5 candidates, 4 slots, so exactly one is cut.
    expect(screen.getByText("+1 more limits in Ceiling")).toBeInTheDocument();
  });

  it("opens the full dashboard and dismisses the glance flyout", async () => {
    render(<TaskbarFlyout state={state} />);
    fireEvent.click(screen.getByRole("button", { name: "Open Ceiling" }));

    await waitFor(() => {
      expect(tauriMocks.setSurfaceMode).toHaveBeenCalledWith("popOut", {
        kind: "dashboard",
      });
      expect(tauriMocks.dismissTrayPanel).toHaveBeenCalled();
    });
  });

  it("uses the taskbar provider selection and excludes failed data from reset claims", async () => {
    const failedCodex = provider("codex", "Codex", 99, 15, "Weekly");
    failedCodex.error = "network timeout";
    const claude = provider("claude", "Claude", 25, 212, "Session (5h)");
    claude.secondary = {
      ...claude.primary,
      usedPercent: 12,
      remainingPercent: 88,
      resetsAt: new Date(Date.now() + 90 * 60_000).toISOString(),
    };
    providerState.providers = [failedCodex, claude];
    const selectedState = {
      ...state,
      settings: {
        ...state.settings,
        floatBarProviderIds: ["codex", "claude"],
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={selectedState} />);

    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    expect(screen.queryByText("99%")).not.toBeInTheDocument();
    expect(screen.getByText(/^Next reset in 1h/)).toBeInTheDocument();
    await waitFor(() => expect(windowMocks.setSize).toHaveBeenCalled());
  });

  it("does not show enabled providers omitted from the taskbar selection", async () => {
    const selectedState = {
      ...state,
      settings: {
        ...state.settings,
        floatBarProviderIds: ["claude"],
      },
    } as BootstrapState;

    render(<TaskbarFlyout state={selectedState} />);

    expect(screen.getByText("Claude")).toBeInTheDocument();
    expect(screen.queryByText("Codex")).not.toBeInTheDocument();
    await waitFor(() => expect(windowMocks.setSize).toHaveBeenCalled());
  });
});
