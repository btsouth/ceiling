import type {
  PaceSnapshot,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
  WindowAmountBridge,
} from "../types/bridge";

export type CapacityFreshness = "live" | "stale" | "error";

/** Named enforcement state for a window that must not paint as 0% / 100%. */
export type NamedWindowState = "unavailable" | "notEnforced";

export type ConstrainingWindow = {
  id: string;
  label: string;
  window: RateWindowSnapshot;
  /** Money behind this lane, when the provider meters it in currency. */
  amount?: WindowAmountBridge | null;
  /**
   * Set when this window's percent is a placeholder, not a reading.
   * Writers still emit a required primary plus an inactive row for the
   * same identity (SBS-876 / CEILING_UI.md).
   */
  namedState?: NamedWindowState;
};

export type GlanceMeters = {
  /**
   * Account plan pool — overview hero. Null when that window is a named
   * placeholder (unavailable / not enforced) so Overview does not also
   * paint a 0% MeterRow (SBS-876).
   */
  primary: ConstrainingWindow | null;
  /** Compact non-primary lanes shown beneath the hero. */
  companions: ConstrainingWindow[];
};

export type ProviderGlanceStatus = "ok" | "warning" | "exhausted" | "error";

export type ActivePromoBoost = {
  id: string;
  title: string;
  description: string;
};

const STALE_AFTER_MS = 10 * 60 * 1000;
/** Companion lanes appear on overview when used reaches this share. */
export const GLANCE_COMPANION_HOT_PERCENT = 70;

/**
 * Inactive-row ids that mark `primary` as a placeholder, not a reading.
 *
 * Match by id only — never by title. Codex can report a real Weekly
 * primary alongside an unavailable Weekly inactive row with a different
 * id (`weekly`); title matching would hide that 51% reading (SBS-876).
 *
 * Sweep: writers that emit 0% primary AND an inactive row for that same
 * window — Cursor (`cursor-plan` unavailable, `cursor-monthly` notEnforced)
 * and Claude (`claude-session` unavailable when five_hour/utilization is
 * absent, SBS-1040).
 */
const PRIMARY_PLACEHOLDER_IDS = new Set([
  "cursor-plan",
  "cursor-monthly",
  "claude-session",
]);

export function isPrimaryPlaceholderId(id: string): boolean {
  return PRIMARY_PLACEHOLDER_IDS.has(id);
}

/**
 * When writers must still emit a primary window, they may also emit an
 * inactiveRateWindows row for that same identity. The percent on primary
 * is then a placeholder, not a reading (SBS-876 / CEILING_UI.md).
 *
 * Missing `state` on an inactive row is notEnforced (existing back-compat).
 */
export function primaryNamedState(
  provider: ProviderUsageSnapshot,
): NamedWindowState | null {
  const hit = (provider.inactiveRateWindows ?? []).find((row) =>
    PRIMARY_PLACEHOLDER_IDS.has(row.id),
  );
  if (!hit) return null;
  return hit.state ?? "notEnforced";
}

function primaryConstrainingWindow(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow {
  return {
    id: "primary",
    label: provider.primaryLabel?.trim() || "Plan",
    window: provider.primary,
    namedState: primaryNamedState(provider) ?? undefined,
  };
}

/** A window you have already hit stops work no matter what the others read. */
function isBlocking(window: RateWindowSnapshot): boolean {
  return window.isExhausted || window.usedPercent >= 100;
}

/** Reset instant in ms; windows without a usable reset sort last. */
function resetAtMs(window: RateWindowSnapshot): number {
  const parsed = window.resetsAt ? Date.parse(window.resetsAt) : Number.NaN;
  return Number.isFinite(parsed) ? parsed : Number.POSITIVE_INFINITY;
}

/**
 * Whether `candidate` is a more relevant thing to surface than `best`.
 *
 * Blended priority, in order:
 *  1. Blocking windows first, because an exhausted lane is what actually stops
 *     you even when another lane reads higher pressure.
 *  2. Then the highest used percent, i.e. closest to its ceiling.
 *  3. Then the soonest reset, so a tie resolves toward the one that bites first.
 *
 * Cursor does **not** use this path (see `cursorStripWindow`): Auto and API are
 * parallel pools, so a maxed API bar must not hide Auto capacity on the strip.
 */
function outranks(
  candidate: ConstrainingWindow,
  best: ConstrainingWindow,
): boolean {
  const candidateBlocking = isBlocking(candidate.window);
  const bestBlocking = isBlocking(best.window);
  if (candidateBlocking !== bestBlocking) return candidateBlocking;
  if (candidate.window.usedPercent !== best.window.usedPercent) {
    return candidate.window.usedPercent > best.window.usedPercent;
  }
  return resetAtMs(candidate.window) < resetAtMs(best.window);
}

/**
 * Cursor Auto + API lanes only. Plan/Monthly is a blend and is not used for the
 * one-number strip when these actionable pools exist.
 */
function cursorActionableWindows(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow[] {
  const out: ConstrainingWindow[] = [];
  if (provider.secondary) {
    out.push({
      id: "secondary",
      label: provider.secondaryLabel?.trim() || "Auto",
      window: provider.secondary,
    });
  }
  for (const extra of provider.extraRateWindows ?? []) {
    if (extra.id !== "cursor-api") continue;
    out.push({
      id: `extra-${extra.id}`,
      label: extra.title?.trim() || "API",
      window: extra.window,
    });
  }
  return out;
}

function cursorOnDemandWindow(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow | null {
  const extra = (provider.extraRateWindows ?? []).find(
    (candidate) => candidate.id === "cursor-on-demand",
  );
  if (!extra) return null;
  return {
    id: `extra-${extra.id}`,
    label: extra.title?.trim() || "On-demand",
    window: extra.window,
    amount: extra.amount,
  };
}

function cursorOnDemandIsActive(
  provider: ProviderUsageSnapshot,
  onDemand: ConstrainingWindow,
): boolean {
  if ((onDemand.amount?.used ?? 0) > 0) return true;
  const actionable = cursorActionableWindows(provider);
  return actionable.length > 0
    ? hottestWithRoom(actionable) === null
    : isBlocking(provider.primary);
}

/** Hottest non-exhausted window, then soonest reset on a used-% tie. */
function hottestWithRoom(
  windows: ConstrainingWindow[],
): ConstrainingWindow | null {
  let best: ConstrainingWindow | null = null;
  for (const candidate of windows) {
    if (isBlocking(candidate.window)) continue;
    if (
      !best ||
      candidate.window.usedPercent > best.window.usedPercent ||
      (candidate.window.usedPercent === best.window.usedPercent &&
        resetAtMs(candidate.window) < resetAtMs(best.window))
    ) {
      best = candidate;
    }
  }
  return best;
}

/** Among exhausted windows, prefer the one that resets soonest. */
function soonestExhausted(
  windows: ConstrainingWindow[],
): ConstrainingWindow | null {
  let best: ConstrainingWindow | null = null;
  for (const candidate of windows) {
    if (!isBlocking(candidate.window)) continue;
    if (
      !best ||
      resetAtMs(candidate.window) < resetAtMs(best.window) ||
      (resetAtMs(candidate.window) === resetAtMs(best.window) &&
        candidate.window.usedPercent > best.window.usedPercent)
    ) {
      best = candidate;
    }
  }
  return best;
}

/**
 * Cursor strip / taskbar readout: show **actionable remaining** capacity.
 *
 * Auto and API are parallel product pools. A maxed API lane does not stop Auto,
 * so the strip prefers the hottest lane that still has room. Only when every
 * actionable lane is exhausted do we surface an exhausted bar (soonest reset).
 * Plan/Monthly never wins the strip when Auto or API is present.
 */
function cursorStripWindow(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow {
  const actionable = cursorActionableWindows(provider);
  const onDemand = cursorOnDemandWindow(provider);
  // Once real-money billing starts it is the most important glance signal,
  // even if Cursor's included-pool counters have not advanced in lockstep.
  if (onDemand && (onDemand.amount?.used ?? 0) > 0) return onDemand;
  if (actionable.length > 0) {
    const withRoom = hottestWithRoom(actionable);
    if (withRoom) return withRoom;
    // Included Auto/API capacity is gone. Show the lane Cursor will bill next,
    // including the useful $0-at-the-boundary state.
    if (onDemand) return onDemand;
    const exhausted = soonestExhausted(actionable);
    if (exhausted) return exhausted;
  }
  if (onDemand && cursorOnDemandIsActive(provider, onDemand)) return onDemand;
  // Plan/Monthly fallback. If that window is a placeholder, carry namedState
  // so the strip does not paint a bare 0% / 100% bar (SBS-876).
  return primaryConstrainingWindow(provider);
}

/**
 * Claude lanes that cap ONE model inside an allowance rather than the
 * allowance itself. Maxing one doesn't stop you working, it means you use a
 * different model — the same parallel-pool shape as Cursor's Auto/API.
 *
 * Claude reports these two different ways and both must be caught:
 *  - the scoped extras, `claude-weekly-scoped-{model}` ("Fable only"), and
 *  - the seven-day Opus/Sonnet cap, which lands in the generic `model` slot.
 *
 * Claude-only on purpose. Other providers put real pools in `model` — Codex's
 * code review, Gemini's Pro quota — and those must keep competing.
 */
function isModelScopedLane(providerId: string, id: string): boolean {
  if (providerId !== "claude") return false;
  return id === "model" || id.startsWith("extra-claude-weekly-scoped-");
}

/**
 * Pick the window actually constraining this provider (see `outranks`).
 *
 * Model-scoped sub-limits are excluded: they are not what stops your work, and
 * the one-number strip has room for exactly one lane. Letting them compete meant
 * a maxed "Fable only" won on the blocking-first rule and took over the whole
 * Claude pill — number, label, and tone — hiding a Session and Weekly that still
 * had capacity. They stay visible everywhere that lists every window: provider
 * detail, the taskbar flyout, the tray menu, and the Activity timeline.
 */
export function constrainingWindow(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow {
  if (provider.providerId === "cursor") {
    return cursorStripWindow(provider);
  }

  let best: ConstrainingWindow = primaryConstrainingWindow(provider);

  for (const candidate of nonPrimaryWindows(provider)) {
    if (isModelScopedLane(provider.providerId, candidate.id)) continue;
    if (outranks(candidate, best)) {
      best = candidate;
    }
  }

  return best;
}

/** Mirrors `format_currency` in `rust/src/core/usage_snapshot.rs`. */
function formatAmount(value: number, currencyCode: string): string {
  const safe = Number.isFinite(value) ? Math.max(0, value) : 0;
  switch (currencyCode.toUpperCase()) {
    case "USD":
      return `$${safe.toFixed(2)}`;
    case "EUR":
      return `€${safe.toFixed(2)}`;
    case "GBP":
      return `£${safe.toFixed(2)}`;
    default:
      return `${safe.toFixed(2)} ${currencyCode}`;
  }
}

/**
 * Headline for a currency-billed lane on a one-number strip.
 *
 * Mirrors `strip_amount_label` in `taskbar_widget.rs` so the native taskbar tile
 * and the floating bar read the same. Uncapped spend has no headroom figure, so
 * "show remaining" falls back to spend-to-date rather than going blank (SBS-191).
 */
export function stripAmountLabel(
  amount: WindowAmountBridge,
  showAsUsed: boolean,
): string {
  if (showAsUsed || amount.limit == null) return amount.formattedUsed;
  return formatAmount(amount.limit - amount.used, amount.currencyCode);
}

/**
 * Lanes that define a plan alongside its hero, listed in display order and
 * shown on overview however quiet they are:
 *  - Cursor's Auto and API are distinct allowances users need to compare.
 *  - Claude's Weekly sits beside the 5-hour session; both cap the subscription.
 *  - OpenCode Go bills against rolling, weekly, and monthly ceilings at once, so
 *    hiding weekly leaves a gap between the two windows that do show.
 */
const PINNED_COMPANION_IDS: Record<string, string[]> = {
  cursor: ["secondary", "extra-cursor-api"],
  claude: ["secondary"],
  opencodego: ["secondary", "tertiary"],
};

/**
 * Overview glance model: primary plan pool as hero, plus compact companion
 * lanes. Providers in `PINNED_COMPANION_IDS` always show their defining lanes;
 * every other provider keeps the single hottest materially constrained lane.
 * Clicking never toggles meters — detail mode lists every window.
 */
export function glanceMeters(provider: ProviderUsageSnapshot): GlanceMeters {
  // Fetch/decode failure is an error snapshot whose primary is a dummy 0%.
  // Overview must not paint that as empty/healthy (SBS-1061).
  if (provider.error) {
    return { primary: null, companions: [] };
  }
  const named = primaryNamedState(provider);
  const primary = named ? null : primaryConstrainingWindow(provider);

  const candidates = nonPrimaryWindows(provider);
  const pinned = PINNED_COMPANION_IDS[provider.providerId];
  if (pinned) {
    const companions = pinned
      .map((id) => candidates.find((candidate) => candidate.id === id))
      .filter((candidate): candidate is ConstrainingWindow => Boolean(candidate));
    if (provider.providerId === "cursor") {
      const onDemand = candidates.find(
        (candidate) => candidate.id === "extra-cursor-on-demand",
      );
      if (onDemand && cursorOnDemandIsActive(provider, onDemand)) {
        companions.push(onDemand);
      }
    }
    return {
      primary,
      companions,
    };
  }

  let companion: ConstrainingWindow | null = null;
  for (const candidate of candidates) {
    // No real primary: do not compare against a placeholder 0%. A companion
    // is hot only on its own used percent (SBS-876).
    const hot = primary
      ? isCompanionHot(candidate.window, primary.window)
      : candidate.window.usedPercent >= GLANCE_COMPANION_HOT_PERCENT;
    if (!hot) continue;
    if (
      !companion ||
      candidate.window.usedPercent > companion.window.usedPercent
    ) {
      companion = candidate;
    }
  }

  return { primary, companions: companion ? [companion] : [] };
}

function isCompanionHot(
  companion: RateWindowSnapshot,
  primary: RateWindowSnapshot,
): boolean {
  return (
    companion.usedPercent >= GLANCE_COMPANION_HOT_PERCENT ||
    companion.usedPercent >= primary.usedPercent
  );
}

function nonPrimaryWindows(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow[] {
  const out: ConstrainingWindow[] = [];
  const push = (
    id: string,
    label: string | null | undefined,
    window: RateWindowSnapshot | null | undefined,
    fallback: string,
    amount?: WindowAmountBridge | null,
  ) => {
    if (!window) return;
    out.push({
      id,
      label: label?.trim() || fallback,
      window,
      amount,
    });
  };

  push("secondary", provider.secondaryLabel, provider.secondary, "Weekly");
  push("model", null, provider.modelSpecific, "Model");
  push("tertiary", provider.tertiaryLabel, provider.tertiary, "Extra");
  for (const extra of provider.extraRateWindows ?? []) {
    if (extra.id === "reset-credits") continue;
    push(`extra-${extra.id}`, extra.title, extra.window, extra.title, extra.amount);
  }
  return out;
}

/**
 * Provider-reported banked resets, including legacy cached snapshots. Returns
 * the observed count — `0` is a real reading and stays distinct from `null`,
 * which means the provider does not report resets (or the fetch is unknown).
 */
export function resetCreditsAvailable(
  provider: ProviderUsageSnapshot,
): number | null {
  if (
    typeof provider.resetCreditsAvailable === "number" &&
    provider.resetCreditsAvailable >= 0
  ) {
    return Math.floor(provider.resetCreditsAvailable);
  }
  const legacy = provider.extraRateWindows?.find(
    (window) => window.id === "reset-credits",
  );
  const match = legacy?.window.resetDescription?.match(/\d+/);
  return match ? Number(match[0]) : null;
}

/**
 * Provider-reported banked resets for the persistent indicator. Codex and Grok
 * are the only providers with this concept, so the chip stays scoped to them
 * and always shows (including the `0` state) whenever we have a trustworthy
 * reading.
 */
export function bankedResetCredits(
  provider: ProviderUsageSnapshot,
): number | null {
  if (provider.providerId !== "codex" && provider.providerId !== "grok") {
    return null;
  }
  return resetCreditsAvailable(provider);
}

/**
 * Every measured rate window for a provider (plan pool + session/weekly/model/
 * extra lanes), each carrying its display label. Powers the Activity timeline,
 * which enumerates all reset windows across providers rather than just the
 * glance hero + companion.
 */
export function allMeasuredWindows(
  provider: ProviderUsageSnapshot,
): ConstrainingWindow[] {
  if (provider.error) {
    return [];
  }
  // A placeholder primary is not a reading — Activity must not list a fake
  // 0% Plan (SBS-876).
  if (primaryNamedState(provider)) {
    return nonPrimaryWindows(provider);
  }
  return [primaryConstrainingWindow(provider), ...nonPrimaryWindows(provider)];
}

/** Grid / glance status chip from constraining pressure. */
export function providerGlanceStatus(
  provider: ProviderUsageSnapshot,
): ProviderGlanceStatus {
  if (provider.error) return "error";
  const constraining = constrainingWindow(provider);
  // A named-state window is not "ok because 0%" and not exhausted. Fetch
  // succeeded; the window is named, not a quota. Unavailable is not error
  // (provider.error is the fetch-failed path). If another measured window
  // is applying pressure, that window still ranks the status (SBS-876).
  if (constraining.namedState) {
    const others = allMeasuredWindows(provider);
    let hottest: ConstrainingWindow | null = null;
    for (const candidate of others) {
      if (
        !hottest ||
        candidate.window.usedPercent > hottest.window.usedPercent
      ) {
        hottest = candidate;
      }
    }
    if (!hottest) return "ok";
    if (hottest.window.isExhausted || hottest.window.usedPercent >= 100) {
      return "exhausted";
    }
    if (hottest.window.usedPercent > 80) return "warning";
    return "ok";
  }
  if (
    constraining.window.isExhausted ||
    constraining.window.usedPercent >= 100
  ) {
    return "exhausted";
  }
  if (constraining.window.usedPercent > 80) return "warning";
  return "ok";
}

/**
 * Strip/flyout freshness from the provider timestamp and error only:
 * error > stale > live. Inactive/lifted windows are surfaced as their own
 * `inactiveRateWindows` rows, never as provider-level freshness (SOU-152).
 */
export function capacityFreshness(
  provider: ProviderUsageSnapshot,
  nowMs = Date.now(),
): CapacityFreshness {
  if (provider.error) return "error";
  const updated = Date.parse(provider.updatedAt);
  if (Number.isFinite(updated) && nowMs - updated > STALE_AFTER_MS) {
    return "stale";
  }
  return "live";
}

export type CalmPaceTone = "steady" | "watch";
export type CalmPaceState = { label: string; tone: CalmPaceTone };
export type CalmPresentation = {
  /** Trustworthy pace state, or null when pace isn't fresh + supported. */
  pace: CalmPaceState | null;
  /** The window whose reset + label headline the calm pill. */
  window: ConstrainingWindow;
  /** Whether that window exposes a reset (a time or a description). */
  hasReset: boolean;
  /** No pace and no reset — callers fall back to the exact percentage. */
  showExactFallback: boolean;
};

/**
 * Compact human duration for the pace pill: "under 1m", "42m", "1h 20m",
 * "2d 3h". Kept short so it fits the float-bar pill.
 */
export function formatShortDuration(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  if (total < 60) return "under 1m";
  const minutes = Math.round(total / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const restMinutes = minutes % 60;
  if (hours < 24) return restMinutes > 0 ? `${hours}h ${restMinutes}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  const restHours = hours % 24;
  return restHours > 0 ? `${days}d ${restHours}h` : `${days}d`;
}

/**
 * Trustworthy pace state for Calm mode, or null when there isn't enough signal.
 * Never invents an "on pace" state (SOU-178): it only speaks when the provider
 * actually reports usable pace data. When the current pace will NOT last to the
 * reset, surface the concrete time left ("~42m left") instead of a vague
 * "Running low", since the number is already computed (SOU-274). "On pace"
 * already carries the reset-aware meaning: the pace lasts until the window
 * resets.
 */
function calmPaceState(pace: PaceSnapshot | null): CalmPaceState | null {
  if (!pace) return null;
  // Data-backed and reassuring: the current pace lasts to the reset.
  if (pace.willLastToReset) return { label: "On pace", tone: "steady" };
  // Only warn when there is a real, finite estimate of running out; otherwise
  // stay silent rather than fabricate a state. Show the concrete ETA.
  if (
    typeof pace.etaSeconds === "number" &&
    Number.isFinite(pace.etaSeconds) &&
    pace.etaSeconds > 0
  ) {
    // Drop the "~" for the sub-minute sentinel so it doesn't read "~under 1m".
    const label =
      pace.etaSeconds < 60
        ? "under 1m left"
        : `~${formatShortDuration(pace.etaSeconds)} left`;
    return { label, tone: "watch" };
  }
  return null;
}

/**
 * What Calm mode surfaces for a provider: a trustworthy pace state plus the
 * next reset of the displayed window, with exact percentages left to expand.
 * Pace is claimed only when the snapshot is live and the provider reports
 * usable pace data. When pace is unavailable we fall back to the next reset;
 * when that is missing too, callers show the exact percentage instead.
 */
export function calmPresentation(
  provider: ProviderUsageSnapshot,
  window: ConstrainingWindow,
  nowMs: number = Date.now(),
): CalmPresentation {
  const fresh = capacityFreshness(provider, nowMs) === "live";
  const pace = fresh && !provider.error ? calmPaceState(provider.pace) : null;
  const hasReset = Boolean(
    window.window.resetsAt || window.window.resetDescription,
  );
  return { pace, window, hasReset, showExactFallback: !pace && !hasReset };
}

/** Boost promos that affect glance surfaces (strip / overview hero). */
export function activePromoBoosts(
  provider: ProviderUsageSnapshot,
): ActivePromoBoost[] {
  return (provider.promoSignals ?? [])
    .filter((signal) => signal.kind === "boost")
    .map((signal) => ({
      id: signal.id,
      title: signal.title,
      description: signal.description,
    }));
}

/** Quieter inclusion notes for detail / overview meta only. */
export function activePromoInclusions(
  provider: ProviderUsageSnapshot,
): ActivePromoBoost[] {
  return (provider.promoSignals ?? [])
    .filter((signal) => signal.kind === "inclusion")
    .map((signal) => ({
      id: signal.id,
      title: signal.title,
      description: signal.description,
    }));
}
