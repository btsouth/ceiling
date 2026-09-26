import { describe, it, expect, vi } from "vitest";
import { formatRelativeUpdated } from "./relativeTime";
import type { LocaleKey } from "../i18n/keys";

// Simple translator stub — echoes the key plus whatever replacement the
// helper substitutes into the "{}" placeholder so we can assert structure.
const t = (key: LocaleKey): string => {
  switch (key) {
    case "NeverUpdated":
      return "Never";
    case "UpdatedJustNow":
      return "just now";
    case "UpdatedMinuteAgo":
      return "{} minute ago";
    case "UpdatedMinutesAgo":
      return "{} minutes ago";
    case "UpdatedHourAgo":
      return "{} hour ago";
    case "UpdatedHoursAgo":
      return "{} hours ago";
    case "UpdatedDayAgo":
      return "{} day ago";
    case "UpdatedDaysAgo":
      return "{} days ago";
    default:
      return key;
  }
};

describe("formatRelativeUpdated", () => {
  const NOW = Date.parse("2024-06-01T12:00:00Z");

  it("returns 'Never' when timestamp is null or undefined", () => {
    expect(formatRelativeUpdated(null, t, NOW)).toBe("Never");
    expect(formatRelativeUpdated(undefined, t, NOW)).toBe("Never");
  });

  it("treats future timestamps as 'just now' (clamps negative diffs)", () => {
    expect(formatRelativeUpdated(NOW + 60_000, t, NOW)).toBe("just now");
  });

  it("uses 'just now' for sub-minute diffs", () => {
    expect(formatRelativeUpdated(NOW - 1_000, t, NOW)).toBe("just now");
    expect(formatRelativeUpdated(NOW - 59_000, t, NOW)).toBe("just now");
  });

  it("renders minutes for sub-hour diffs", () => {
    expect(formatRelativeUpdated(NOW - 2 * 60_000, t, NOW)).toBe(
      "2 minutes ago",
    );
    expect(formatRelativeUpdated(NOW - 59 * 60_000, t, NOW)).toBe(
      "59 minutes ago",
    );
  });

  it("uses the singular for exactly one minute", () => {
    expect(formatRelativeUpdated(NOW - 60_000, t, NOW)).toBe("1 minute ago");
    expect(formatRelativeUpdated(NOW - 119_000, t, NOW)).toBe("1 minute ago");
    expect(formatRelativeUpdated(NOW - 120_000, t, NOW)).toBe("2 minutes ago");
  });

  it("renders hours for sub-day diffs", () => {
    expect(formatRelativeUpdated(NOW - 60 * 60_000, t, NOW)).toBe(
      "1 hour ago",
    );
    expect(formatRelativeUpdated(NOW - 119 * 60_000, t, NOW)).toBe(
      "1 hour ago",
    );
    expect(formatRelativeUpdated(NOW - 23 * 3600_000, t, NOW)).toBe(
      "23 hours ago",
    );
  });

  it("renders days beyond 24h", () => {
    expect(formatRelativeUpdated(NOW - 24 * 3600_000, t, NOW)).toBe(
      "1 day ago",
    );
    expect(formatRelativeUpdated(NOW - 9 * 24 * 3600_000, t, NOW)).toBe(
      "9 days ago",
    );
  });

  it("defaults `nowMs` to Date.now()", () => {
    const spy = vi.spyOn(Date, "now").mockReturnValue(NOW);
    try {
      expect(formatRelativeUpdated(NOW - 120_000, t)).toBe("2 minutes ago");
    } finally {
      spy.mockRestore();
    }
  });
});
