import { describe, expect, it } from "vitest";
import {
  allMeasuredWindows,
  capacityFreshness,
  constrainingWindow,
  glanceMeters,
  activePromoBoosts,
  activePromoInclusions,
  primaryNamedState,
  providerGlanceStatus,
  resetCreditsAvailable,
  bankedResetCredits,
  calmPresentation,
  formatShortDuration,
  stripAmountLabel,
} from "./capacityPresentation";
import type {
  PaceSnapshot,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
} from "../types/bridge";

function window(usedPercent: number): RateWindowSnapshot {
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes: null,
    resetsAt: null,
    resetDescription: null,
    isExhausted: usedPercent >= 100,
    reservePercent: null,
    reserveDescription: null,
    reserveWillLastToReset: false,
    reserveEtaSeconds: null,
  };
}

function provider(
  overrides: Partial<ProviderUsageSnapshot> = {},
): ProviderUsageSnapshot {
  return {
    providerId: "cursor",
    displayName: "Cursor",
    primary: window(30),
    primaryLabel: "Monthly",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    inactiveRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "web",
    updatedAt: new Date().toISOString(),
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
    ...overrides,
  };
}

describe("capacityPresentation", () => {
  it("selects the highest used measured window as constraining", () => {
    const snap = provider({
      secondary: window(55),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(10) },
      ],
    });
    const constraining = constrainingWindow(snap);
    expect(constraining.id).toBe("secondary");
    expect(constraining.label).toBe("Auto");
    expect(constraining.window.usedPercent).toBe(55);
  });

  it("surfaces a hot weekly over a freshly reset session (SOU-288)", () => {
    const snap = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(2),
      primaryLabel: "Session (5h)",
      secondary: window(95),
      secondaryLabel: "Weekly",
    });

    const constraining = constrainingWindow(snap);

    expect(constraining.id).toBe("secondary");
    expect(constraining.label).toBe("Weekly");
    expect(constraining.window.usedPercent).toBe(95);
  });

  it("prefers a blocking window over a merely higher-pressure one", () => {
    const exhausted = window(100);
    // Claude uses the blocking-wins path (Cursor uses actionable remaining).
    const snap = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(100),
      primaryLabel: "Session",
      secondary: { ...window(97), isExhausted: false },
      secondaryLabel: "Weekly",
    });
    expect(exhausted.isExhausted).toBe(true);

    // Primary is blocking at 100%, so it outranks the hotter-looking 97% lane.
    expect(constrainingWindow(snap).id).toBe("primary");

    // And a blocking non-primary wins even when primary reads higher.
    const blockedExtra = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(80),
      primaryLabel: "Session",
      extraRateWindows: [
        {
          id: "claude-design",
          title: "Design",
          window: { ...window(40), isExhausted: true },
        },
      ],
    });
    expect(constrainingWindow(blockedExtra).id).toBe("extra-claude-design");
  });

  it("keeps a maxed model-scoped lane off the strip while a pool has room", () => {
    // Reporter case: Claude's weekly Fable sub-limit hits 100% and the strip
    // pill shows "Fable only 100%", hiding a Session and Weekly with capacity.
    // Maxing one model does not stop work — you use another model.
    const fableMaxed = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      secondary: window(12),
      secondaryLabel: "Weekly",
      extraRateWindows: [
        {
          id: "claude-weekly-scoped-fable",
          title: "Fable only",
          window: window(100),
        },
      ],
    });
    expect(constrainingWindow(fableMaxed).label).toBe("Session (5h)");
    expect(constrainingWindow(fableMaxed).window.usedPercent).toBe(34);

    // Not just the blocking case: a merely hot scoped lane must not win either.
    const fableHot = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      secondary: window(12),
      secondaryLabel: "Weekly",
      extraRateWindows: [
        {
          id: "claude-weekly-scoped-opus",
          title: "Opus only",
          window: window(99),
        },
      ],
    });
    expect(constrainingWindow(fableHot).label).toBe("Session (5h)");

    // Claude's seven-day Opus/Sonnet cap does not travel as a scoped extra —
    // it lands in the generic `model` slot — and is the same category.
    const opusMaxed = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      secondary: window(12),
      secondaryLabel: "Weekly",
      modelSpecific: window(100),
    });
    expect(constrainingWindow(opusMaxed).label).toBe("Session (5h)");
    expect(providerGlanceStatus(opusMaxed)).toBe("ok");

    // Claude-only: other providers use `model` for real pools that must bind.
    const codexModel = provider({
      providerId: "codex",
      displayName: "Codex",
      primary: window(20),
      primaryLabel: "Session",
      modelSpecific: window(90),
    });
    expect(constrainingWindow(codexModel).id).toBe("model");
    expect(constrainingWindow(codexModel).window.usedPercent).toBe(90);

    // Real pools still rank normally against each other.
    const weeklyHot = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      secondary: window(91),
      secondaryLabel: "Weekly",
      extraRateWindows: [
        {
          id: "claude-weekly-scoped-sonnet",
          title: "Sonnet only",
          window: window(100),
        },
      ],
    });
    expect(constrainingWindow(weeklyHot).label).toBe("Weekly");
    expect(constrainingWindow(weeklyHot).window.usedPercent).toBe(91);
  });

  it("does not report Claude as exhausted for a maxed model-scoped lane", () => {
    const snap = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      extraRateWindows: [
        {
          id: "claude-weekly-scoped-fable",
          title: "Fable only",
          window: window(100),
        },
      ],
    });
    expect(providerGlanceStatus(snap)).toBe("ok");
  });

  it("still lists model-scoped lanes among all measured windows", () => {
    const snap = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: window(34),
      primaryLabel: "Session (5h)",
      extraRateWindows: [
        {
          id: "claude-weekly-scoped-fable",
          title: "Fable only",
          window: window(100),
        },
      ],
    });
    expect(
      allMeasuredWindows(snap).map((measured) => measured.label),
    ).toContain("Fable only");
  });

  it("breaks an exact tie toward the window that resets first", () => {
    const soon = { ...window(60), resetsAt: "2026-07-21T04:00:00Z" };
    const later = { ...window(60), resetsAt: "2026-07-28T04:00:00Z" };
    const snap = provider({
      providerId: "claude",
      displayName: "Claude",
      primary: later,
      primaryLabel: "Weekly",
      secondary: soon,
      secondaryLabel: "Session",
    });

    expect(constrainingWindow(snap).label).toBe("Session");
  });

  it("Cursor strip prefers hottest Auto/API that still has room (not Plan)", () => {
    // Reporter case: API 100% while Auto ~60% still has room → show Auto.
    const apiMaxed = provider({
      primary: window(40),
      primaryLabel: "Monthly",
      secondary: window(60),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(100) },
      ],
    });
    expect(constrainingWindow(apiMaxed).label).toBe("Auto");
    expect(constrainingWindow(apiMaxed).window.usedPercent).toBe(60);

    // Symmetric: Auto maxed, API still useful → show API.
    const autoMaxed = provider({
      primary: window(40),
      primaryLabel: "Monthly",
      secondary: window(100),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(40) },
      ],
    });
    expect(constrainingWindow(autoMaxed).label).toBe("API");
    expect(constrainingWindow(autoMaxed).window.usedPercent).toBe(40);

    // Both open: hottest wins (not Plan even if Plan is hotter).
    const bothOpen = provider({
      primary: window(90),
      primaryLabel: "Monthly",
      secondary: window(55),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(70) },
      ],
    });
    expect(constrainingWindow(bothOpen).label).toBe("API");
    expect(constrainingWindow(bothOpen).window.usedPercent).toBe(70);
  });

  it("Cursor strip falls back to soonest-reset exhausted lane when both are maxed", () => {
    const soon = { ...window(100), resetsAt: "2026-07-21T04:00:00Z" };
    const later = { ...window(100), resetsAt: "2026-07-28T04:00:00Z" };
    const snap = provider({
      primary: window(50),
      primaryLabel: "Monthly",
      secondary: later,
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: soon },
      ],
    });
    expect(constrainingWindow(snap).label).toBe("API");
    expect(constrainingWindow(snap).window.usedPercent).toBe(100);
  });

  it("Cursor strip falls back to Plan when Auto/API lanes are absent", () => {
    const snap = provider({
      primary: window(42),
      primaryLabel: "Monthly",
      secondary: null,
      extraRateWindows: [],
    });
    expect(constrainingWindow(snap).id).toBe("primary");
    expect(constrainingWindow(snap).label).toBe("Monthly");
    expect(constrainingWindow(snap).window.usedPercent).toBe(42);
  });

  it("keeps Cursor plan as hero and shows reported Auto and API companions", () => {
    const meters = glanceMeters(
      provider({
        primary: window(62),
        primaryLabel: "Monthly",
        secondary: window(90),
        secondaryLabel: "Auto",
        extraRateWindows: [
          { id: "cursor-api", title: "API", window: window(12) },
        ],
      }),
    );
    expect(meters.primary?.label).toBe("Monthly");
    expect(meters.primary?.window.usedPercent).toBe(62);
    expect(meters.companions.map((meter) => meter.label)).toEqual(["Auto", "API"]);
    expect(meters.companions.map((meter) => meter.window.usedPercent)).toEqual([
      90, 12,
    ]);
  });

  it("keeps Cursor companion order stable when API is hotter than Auto", () => {
    const meters = glanceMeters(
      provider({
        primary: window(40),
        primaryLabel: "Monthly",
        secondary: window(55),
        secondaryLabel: "Auto",
        extraRateWindows: [
          { id: "cursor-api", title: "API", window: window(88) },
        ],
      }),
    );
    expect(meters.companions.map((meter) => meter.label)).toEqual(["Auto", "API"]);
    expect(meters.companions.map((meter) => meter.window.usedPercent)).toEqual([
      55, 88,
    ]);
  });

  it("surfaces active Cursor on-demand spend after included usage is exhausted", () => {
    const snap = provider({
      primary: window(100),
      primaryLabel: "Plan",
      secondary: window(100),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(100) },
        {
          id: "cursor-on-demand",
          title: "On-demand",
          window: window(56),
          amount: {
            used: 1002.16,
            limit: 1800,
            currencyCode: "USD",
            formattedUsed: "$1,002.16",
            formattedLimit: "$1,800.00",
          },
        },
      ],
    });

    const strip = constrainingWindow(snap);
    expect(strip.label).toBe("On-demand");
    expect(strip.window.usedPercent).toBe(56);

    const meters = glanceMeters(snap);
    expect(meters.companions.map((meter) => meter.label)).toEqual([
      "Auto",
      "API",
      "On-demand",
    ]);
    expect(meters.companions[2].amount?.formattedUsed).toBe("$1,002.16");
  });

  it("keeps unused Cursor on-demand out of a healthy overview", () => {
    const snap = provider({
      primary: window(40),
      primaryLabel: "Plan",
      secondary: window(20),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(10) },
        {
          id: "cursor-on-demand",
          title: "On-demand",
          window: window(0),
          amount: {
            used: 0,
            limit: 1800,
            currencyCode: "USD",
            formattedUsed: "$0.00",
            formattedLimit: "$1,800.00",
          },
        },
      ],
    });

    expect(glanceMeters(snap).companions.map((meter) => meter.label)).toEqual([
      "Auto",
      "API",
    ]);
    expect(constrainingWindow(snap).label).toBe("Auto");
  });

  it("keeps unused Cursor on-demand hidden while Auto still has room", () => {
    const snap = provider({
      primary: window(100),
      primaryLabel: "Plan",
      secondary: window(20),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(100) },
        {
          id: "cursor-on-demand",
          title: "On-demand",
          window: window(0),
          amount: {
            used: 0,
            limit: 1800,
            currencyCode: "USD",
            formattedUsed: "$0.00",
            formattedLimit: "$1,800.00",
          },
        },
      ],
    });

    expect(glanceMeters(snap).companions.map((meter) => meter.label)).toEqual([
      "Auto",
      "API",
    ]);
    expect(constrainingWindow(snap).label).toBe("Auto");
  });

  it("shows the zero-spend boundary when every Cursor lane is exhausted", () => {
    const snap = provider({
      primary: window(50),
      primaryLabel: "Plan",
      secondary: window(100),
      secondaryLabel: "Auto",
      extraRateWindows: [
        { id: "cursor-api", title: "API", window: window(100) },
        {
          id: "cursor-on-demand",
          title: "On-demand",
          window: window(0),
          amount: {
            used: 0,
            limit: 1800,
            currencyCode: "USD",
            formattedUsed: "$0.00",
            formattedLimit: "$1,800.00",
          },
        },
      ],
    });

    expect(constrainingWindow(snap).label).toBe("On-demand");
    expect(glanceMeters(snap).companions.map((meter) => meter.label)).toEqual([
      "Auto",
      "API",
      "On-demand",
    ]);
  });

  it("keeps Claude weekly visible even when it is quieter than the session", () => {
    const meters = glanceMeters(
      provider({
        providerId: "claude",
        displayName: "Claude",
        primary: window(45),
        primaryLabel: "Session",
        secondary: window(20),
        secondaryLabel: "Weekly",
      }),
    );
    expect(meters.companions.map((meter) => meter.label)).toEqual(["Weekly"]);
    expect(meters.companions[0].window.usedPercent).toBe(20);
  });

  it("keeps OpenCode Go weekly beside its rolling and monthly ceilings", () => {
    // The three windows are one plan: a quiet weekly must not drop out and
    // leave a gap between the rolling hero and the monthly lane.
    const meters = glanceMeters(
      provider({
        providerId: "opencodego",
        displayName: "OpenCode Go",
        primary: window(34),
        primaryLabel: "Rolling (5h)",
        secondary: window(32),
        secondaryLabel: "Weekly",
        tertiary: window(57),
        tertiaryLabel: "Monthly",
      }),
    );
    expect(meters.companions.map((meter) => meter.label)).toEqual([
      "Weekly",
      "Monthly",
    ]);
    expect(meters.companions.map((meter) => meter.window.usedPercent)).toEqual([
      32, 57,
    ]);
  });

  it("reports glance status from constraining pressure", () => {
    expect(providerGlanceStatus(provider({ error: "nope" }))).toBe("error");
    expect(
      providerGlanceStatus(
        provider({
          primary: window(10),
          secondary: window(95),
          secondaryLabel: "Auto",
        }),
      ),
    ).toBe("warning");
    expect(
      providerGlanceStatus(
        provider({
          primary: window(100),
        }),
      ),
    ).toBe("exhausted");
  });

  it("reports freshness precedence error > stale > live", () => {
    expect(capacityFreshness(provider({ error: "fail" }))).toBe("error");
    expect(
      capacityFreshness(
        provider({
          updatedAt: new Date(Date.now() - 20 * 60 * 1000).toISOString(),
        }),
      ),
    ).toBe("stale");
    expect(capacityFreshness(provider())).toBe("live");
  });

  it("keeps live freshness when only some windows are inactive (SOU-152)", () => {
    // Inactive windows are surfaced as their own rows, never as a
    // provider-level "lifted" freshness state.
    const inactiveRateWindows = [
      {
        id: "cursor-auto",
        title: "Auto",
        description: "Not currently enforced by Cursor",
      },
    ];
    expect(capacityFreshness(provider({ inactiveRateWindows }))).toBe("live");
    // A stale timestamp still wins over inactive windows.
    expect(
      capacityFreshness(
        provider({
          updatedAt: new Date(Date.now() - 20 * 60 * 1000).toISOString(),
          inactiveRateWindows,
        }),
      ),
    ).toBe("stale");
  });

  it("separates boost promos from inclusion notes", () => {
    const snap = provider({
      promoSignals: [
        {
          id: "claude-weekly-promo",
          kind: "boost",
          title: "Weekly promo",
          description: "Temporary promotional weekly capacity",
        },
        {
          id: "cursor-grok",
          kind: "inclusion",
          title: "Grok in Auto",
          description: "Model included in Auto pool",
        },
      ],
    });
    expect(activePromoBoosts(snap).map((p) => p.id)).toEqual([
      "claude-weekly-promo",
    ]);
    expect(activePromoInclusions(snap).map((p) => p.id)).toEqual(["cursor-grok"]);
  });

  it("reads reset availability without treating it as a usage meter", () => {
    const snap = provider({
      resetCreditsAvailable: 1,
      extraRateWindows: [
        {
          id: "reset-credits",
          title: "Reset credits",
          window: { ...window(0), resetDescription: "1 reset credit available" },
        },
      ],
    });
    expect(resetCreditsAvailable(snap)).toBe(1);
    expect(glanceMeters(snap).companions).toEqual([]);
  });

  it("reports a known zero banked-reset count instead of hiding it", () => {
    expect(resetCreditsAvailable(provider({ resetCreditsAvailable: 0 }))).toBe(0);
  });

  it("exposes banked resets only for Codex and Grok, including the zero state", () => {
    expect(
      bankedResetCredits(provider({ providerId: "codex", resetCreditsAvailable: 0 })),
    ).toBe(0);
    expect(
      bankedResetCredits(provider({ providerId: "codex", resetCreditsAvailable: 3 })),
    ).toBe(3);
    expect(
      bankedResetCredits(provider({ providerId: "grok", resetCreditsAvailable: 0 })),
    ).toBe(0);
    expect(
      bankedResetCredits(provider({ providerId: "grok", resetCreditsAvailable: 1 })),
    ).toBe(1);
    // Another provider reporting the field must never light up the chip.
    expect(
      bankedResetCredits(provider({ providerId: "cursor", resetCreditsAvailable: 2 })),
    ).toBeNull();
  });

  describe("stripAmountLabel", () => {
    const capped = {
      used: 1112.92,
      limit: 1800,
      currencyCode: "USD",
      formattedUsed: "$1112.92",
      formattedLimit: "$1800.00",
    };

    it("leads with spend, not the fraction of the cap", () => {
      expect(stripAmountLabel(capped, true)).toBe("$1112.92");
    });

    it("reports headroom in currency when showing remaining", () => {
      expect(stripAmountLabel(capped, false)).toBe("$687.08");
    });

    it("falls back to spend when on-demand is uncapped", () => {
      // No denominator means no headroom exists. Blanking here is what hid
      // spend from uncapped users entirely (SBS-191).
      expect(
        stripAmountLabel(
          {
            used: 42.5,
            limit: null,
            currencyCode: "USD",
            formattedUsed: "$42.50",
            formattedLimit: null,
          },
          false,
        ),
      ).toBe("$42.50");
    });

    it("keeps non-USD currencies readable", () => {
      expect(
        stripAmountLabel(
          {
            used: 10,
            limit: 40,
            currencyCode: "EUR",
            formattedUsed: "€10.00",
            formattedLimit: "€40.00",
          },
          false,
        ),
      ).toBe("€30.00");
    });
  });

  describe("calmPresentation", () => {
    const pace = (over: Partial<PaceSnapshot> = {}): PaceSnapshot => ({
      windowLabel: "Weekly",
      stage: "on_track",
      deltaPercent: 0,
      willLastToReset: true,
      etaSeconds: null,
      expectedUsedPercent: 40,
      actualUsedPercent: 40,
      ...over,
    });
    const withReset = (usedPercent: number): RateWindowSnapshot => ({
      ...window(usedPercent),
      resetsAt: new Date(Date.now() + 3_600_000).toISOString(),
    });

    it("shows a steady pace state when fresh pace lasts to reset", () => {
      const snap = provider({ pace: pace({ willLastToReset: true }) });
      const result = calmPresentation(snap, constrainingWindow(snap));
      expect(result.pace).toEqual({ label: "On pace", tone: "steady" });
      expect(result.showExactFallback).toBe(false);
    });

    it("shows the concrete time left when pace will not last to reset", () => {
      const risky = provider({
        pace: pace({ willLastToReset: false, etaSeconds: 3600 }),
      });
      expect(calmPresentation(risky, constrainingWindow(risky)).pace).toEqual({
        label: "~1h left",
        tone: "watch",
      });

      const soon = provider({
        pace: pace({ willLastToReset: false, etaSeconds: 42 * 60 }),
      });
      expect(calmPresentation(soon, constrainingWindow(soon)).pace).toEqual({
        label: "~42m left",
        tone: "watch",
      });

      // Sub-minute drops the "~" so it doesn't read "~under 1m left".
      const imminent = provider({
        pace: pace({ willLastToReset: false, etaSeconds: 30 }),
      });
      expect(calmPresentation(imminent, constrainingWindow(imminent)).pace).toEqual({
        label: "under 1m left",
        tone: "watch",
      });

      // No usable ETA: stay silent rather than invent a state.
      const vague = provider({
        pace: pace({ willLastToReset: false, etaSeconds: null }),
      });
      expect(calmPresentation(vague, constrainingWindow(vague)).pace).toBeNull();
    });

    it("never invents a pace state when pace is missing, stale, or errored", () => {
      const noPace = provider({ pace: null });
      expect(calmPresentation(noPace, constrainingWindow(noPace)).pace).toBeNull();

      const stale = provider({
        pace: pace({ willLastToReset: true }),
        updatedAt: new Date(Date.now() - 60 * 60 * 1000).toISOString(),
      });
      expect(calmPresentation(stale, constrainingWindow(stale)).pace).toBeNull();

      const errored = provider({ pace: pace({ willLastToReset: true }), error: "boom" });
      expect(calmPresentation(errored, constrainingWindow(errored)).pace).toBeNull();
    });

    it("falls back to exact percentage only when neither pace nor a reset exists", () => {
      const noReset = provider({ pace: null, primary: window(30) });
      const bare = calmPresentation(noReset, constrainingWindow(noReset));
      expect(bare.hasReset).toBe(false);
      expect(bare.showExactFallback).toBe(true);

      const withResetSnap = provider({ pace: null, primary: withReset(30) });
      const reset = calmPresentation(withResetSnap, constrainingWindow(withResetSnap));
      expect(reset.hasReset).toBe(true);
      expect(reset.showExactFallback).toBe(false);
    });
  });

  /**
   * SBS-876: Cursor still writes 0% primary when monthly is missing, plus an
   * inactive `cursor-plan` row. Glance readers must treat that percent as a
   * placeholder, not a reading.
   */
  describe("named primary placeholder (SBS-876)", () => {
    const missingPlan = () =>
      provider({
        primary: window(0),
        primaryLabel: "Plan",
        inactiveRateWindows: [
          {
            id: "cursor-plan",
            title: "Plan",
            description: "No usage reported",
            state: "unavailable",
          },
        ],
      });

    it("does not treat a missing Cursor plan as a 0% hero", () => {
      const snap = missingPlan();
      expect(primaryNamedState(snap)).toBe("unavailable");
      expect(glanceMeters(snap).primary).toBeNull();
      expect(constrainingWindow(snap).namedState).toBe("unavailable");
      expect(
        allMeasuredWindows(snap).some(
          (measured) =>
            measured.window.usedPercent === 0 &&
            measured.label.toLowerCase() === "plan",
        ),
      ).toBe(false);
      expect(providerGlanceStatus(snap)).not.toBe("exhausted");
      expect(providerGlanceStatus(snap)).toBe("ok");
    });

    it("keeps glance primary null when Auto is present; strip still prefers Auto", () => {
      const snap = provider({
        primary: window(0),
        primaryLabel: "Plan",
        secondary: window(44),
        secondaryLabel: "Auto",
        inactiveRateWindows: [
          {
            id: "cursor-plan",
            title: "Plan",
            description: "No usage reported",
            state: "unavailable",
          },
        ],
      });
      expect(glanceMeters(snap).primary).toBeNull();
      expect(glanceMeters(snap).companions.map((meter) => meter.label)).toEqual([
        "Auto",
      ]);
      expect(constrainingWindow(snap).label).toBe("Auto");
      expect(constrainingWindow(snap).namedState).toBeUndefined();
      expect(constrainingWindow(snap).window.usedPercent).toBe(44);
    });

    it("surfaces unlimited monthly as notEnforced, not a 0% hero", () => {
      const snap = provider({
        primary: window(0),
        primaryLabel: "Monthly",
        inactiveRateWindows: [
          {
            id: "cursor-monthly",
            title: "Monthly",
            description: "Not currently enforced by Cursor",
            state: "notEnforced",
          },
        ],
      });
      expect(primaryNamedState(snap)).toBe("notEnforced");
      expect(glanceMeters(snap).primary).toBeNull();
      expect(constrainingWindow(snap).namedState).toBe("notEnforced");
      expect(
        allMeasuredWindows(snap).some((measured) => measured.window.usedPercent === 0),
      ).toBe(false);
    });

    it("does not treat a missing Claude session as a 0% hero (SBS-1040)", () => {
      const snap = provider({
        providerId: "claude",
        displayName: "Claude",
        primary: window(0),
        primaryLabel: "Session (5h)",
        secondary: window(23),
        secondaryLabel: "Weekly",
        inactiveRateWindows: [
          {
            id: "claude-session",
            title: "Session (5h)",
            description: "No usage reported",
            state: "unavailable",
          },
        ],
      });
      expect(primaryNamedState(snap)).toBe("unavailable");
      expect(glanceMeters(snap).primary).toBeNull();
      expect(glanceMeters(snap).companions.map((meter) => meter.label)).toEqual([
        "Weekly",
      ]);
      expect(
        allMeasuredWindows(snap).some(
          (measured) =>
            measured.window.usedPercent === 0 &&
            measured.label.toLowerCase().includes("session"),
        ),
      ).toBe(false);
    });

    it("does not invent 0% when Vertex fetch/decode failed (SBS-1061)", () => {
      const snap = provider({
        providerId: "vertexai",
        displayName: "Vertex AI",
        primary: window(0),
        primaryLabel: "Usage",
        error: "Vertex AI Resource Manager request failed: HTTP 500",
      });
      expect(providerGlanceStatus(snap)).toBe("error");
      expect(glanceMeters(snap).primary).toBeNull();
      expect(glanceMeters(snap).companions).toEqual([]);
      expect(allMeasuredWindows(snap)).toEqual([]);
      expect(capacityFreshness(snap)).toBe("error");
    });

    it("still heroes a real 0% plan when no inactive row marks it a placeholder", () => {
      // Unknown is not empty: a genuine 0% reading must stay a 0% hero.
      const snap = provider({
        primary: window(0),
        primaryLabel: "Plan",
        inactiveRateWindows: [],
      });
      expect(primaryNamedState(snap)).toBeNull();
      expect(glanceMeters(snap).primary?.window.usedPercent).toBe(0);
      expect(constrainingWindow(snap).namedState).toBeUndefined();
      expect(constrainingWindow(snap).window.usedPercent).toBe(0);
    });

    it("does not hide a real Weekly primary because an inactive Weekly has the same title", () => {
      // ProviderDetailView.test.tsx Codex fixture: 51% Weekly primary plus
      // an unavailable Weekly row with a different id. Title-only matching
      // would hide the real reading.
      const snap = provider({
        providerId: "codex",
        displayName: "Codex",
        primary: window(51),
        primaryLabel: "Weekly",
        inactiveRateWindows: [
          {
            id: "weekly",
            title: "Weekly",
            description: "Not reported in the latest update",
            state: "unavailable",
          },
        ],
      });
      expect(primaryNamedState(snap)).toBeNull();
      expect(glanceMeters(snap).primary?.window.usedPercent).toBe(51);
      expect(constrainingWindow(snap).window.usedPercent).toBe(51);
      expect(constrainingWindow(snap).namedState).toBeUndefined();
    });
  });

  describe("formatShortDuration", () => {
    it("formats compactly across ranges", () => {
      expect(formatShortDuration(0)).toBe("under 1m");
      expect(formatShortDuration(59)).toBe("under 1m");
      expect(formatShortDuration(60)).toBe("1m");
      expect(formatShortDuration(42 * 60)).toBe("42m");
      expect(formatShortDuration(60 * 60)).toBe("1h");
      expect(formatShortDuration(90 * 60)).toBe("1h 30m");
      expect(formatShortDuration(25 * 3600)).toBe("1d 1h");
      expect(formatShortDuration(48 * 3600)).toBe("2d");
    });

    it("never returns a negative duration", () => {
      expect(formatShortDuration(-500)).toBe("under 1m");
    });
  });
});
