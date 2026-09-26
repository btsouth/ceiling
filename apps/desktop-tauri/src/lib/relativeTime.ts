import type { LocaleKey } from "../i18n/keys";

type Translate = (key: LocaleKey) => string;

/**
 * Render a Unix-ms timestamp as a localized "updated N ago" string,
 * matching the egui `render_advanced_tab` time display.
 *
 * Returns the `NeverUpdated` locale string when `timestampMs` is `null`
 * or `undefined`, mirroring how egui surfaces provider-update timing.
 */
export function formatRelativeUpdated(
  timestampMs: number | null | undefined,
  t: Translate,
  nowMs: number = Date.now(),
): string {
  if (timestampMs == null) {
    return t("NeverUpdated");
  }
  const diffSecs = Math.max(0, Math.floor((nowMs - timestampMs) / 1000));
  if (diffSecs < 60) {
    return t("UpdatedJustNow");
  }
  const diffMins = Math.floor(diffSecs / 60);
  if (diffMins < 60) {
    return countedAgo(diffMins, "UpdatedMinuteAgo", "UpdatedMinutesAgo", t);
  }
  const diffHours = Math.floor(diffMins / 60);
  if (diffHours < 24) {
    return countedAgo(diffHours, "UpdatedHourAgo", "UpdatedHoursAgo", t);
  }
  const diffDays = Math.floor(diffHours / 24);
  return countedAgo(diffDays, "UpdatedDayAgo", "UpdatedDaysAgo", t);
}

/**
 * Pick the singular or plural string for `count`. English, which every
 * bundle without its own translation falls back to, uses the singular for
 * exactly one; Chinese carries the same text under both keys.
 */
function countedAgo(
  count: number,
  singular: LocaleKey,
  plural: LocaleKey,
  t: Translate,
): string {
  return t(count === 1 ? singular : plural).replace("{}", String(count));
}
