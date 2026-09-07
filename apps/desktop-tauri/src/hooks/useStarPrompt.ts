import { useCallback, useEffect, useRef, useState } from "react";
import { getAppInfo, openExternalUrl } from "../lib/tauri";
import {
  hasRealReading,
  readStarPromptState,
  recordStarPromptShown,
  recordStarPromptStarred,
  starPromptReason,
  type StarPromptReason,
} from "../lib/starPrompt";
import type { ProviderUsageSnapshot } from "../types/bridge";

const REPO_URL = "https://github.com/btsouth/ceiling";

/** How often the settle timer is re-checked. Coarse on purpose; nothing here is urgent. */
const TICK_MS = 5_000;

export interface StarPromptController {
  /** Why the prompt is showing, or null when it is not. */
  reason: StarPromptReason | null;
  /** App version, for the post-update copy. Empty until `get_app_info` lands. */
  version: string;
  onStar: () => void;
  onDismiss: () => void;
}

/**
 * Drives the GitHub star prompt (SOU-311). Reads the eligibility rules in
 * `starPrompt.ts`, and once the prompt goes up it stays up until the user
 * answers it — a reading going stale or a provider erroring underneath does not
 * pull the card out from under a click that is already on its way.
 *
 * The ask is counted when the card first appears, not when it is answered, so a
 * user who closes the window without touching it has still had their first ask.
 * Anything else would let the same prompt come back every launch, which is the
 * behaviour this whole feature is trying not to have.
 */
export function useStarPrompt(
  providers: ProviderUsageSnapshot[],
  /** False in surfaces that must never ask (auxiliary windows, empty states). */
  enabled = true,
): StarPromptController {
  const [version, setVersion] = useState<string | null>(null);
  const [reason, setReason] = useState<StarPromptReason | null>(null);
  const [tick, setTick] = useState(0);
  // First moment this session that a real reading was on screen. The settle
  // delay is measured from here rather than from mount, so the wait covers the
  // user actually having numbers to look at and not a spinner.
  const readingSinceRef = useRef<number | null>(null);
  const answeredRef = useRef(false);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    void getAppInfo()
      .then((info) => {
        if (!cancelled) setVersion(info.version);
      })
      // No version means no prompt, which is the correct way to fail here.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  const hasReading = enabled && hasRealReading(providers);
  if (hasReading) {
    readingSinceRef.current ??= Date.now();
  } else if (reason === null) {
    // The reading went away before it had settled — a failed refresh, a
    // provider dropping out. The next one starts its twenty seconds over
    // rather than inheriting credit from a reading that is no longer there.
    // Once the card is up this stops applying; see the effect below.
    readingSinceRef.current = null;
  }

  // Re-evaluate on a slow tick so the settle delay can elapse without needing a
  // provider refresh to happen to land at the right moment.
  useEffect(() => {
    if (!enabled || reason !== null || answeredRef.current) return;
    const timer = window.setInterval(() => setTick((n) => n + 1), TICK_MS);
    return () => window.clearInterval(timer);
  }, [enabled, reason]);

  useEffect(() => {
    if (!enabled || reason !== null || answeredRef.current) return;
    if (!version) return;
    const next = starPromptReason({
      state: readStarPromptState(),
      version,
      hasReading,
      readingSince: readingSinceRef.current,
      now: Date.now(),
    });
    if (!next) return;
    // Count the ask before it becomes visible, and only show it if that count
    // actually persisted. An ask that cannot be recorded is an ask with no cap
    // on it, so a storage layer that will not hold the record means no prompt
    // at all rather than one on every launch.
    if (!recordStarPromptShown(version, Date.now())) return;
    setReason(next);
  }, [enabled, reason, version, hasReading, tick]);

  const onStar = useCallback(() => {
    answeredRef.current = true;
    recordStarPromptStarred();
    setReason(null);
    // A failed launch still ends the asking. The user answered; Ceiling not
    // being able to open a browser is not their problem to be reminded about.
    void openExternalUrl(REPO_URL).catch(() => {});
  }, []);

  const onDismiss = useCallback(() => {
    answeredRef.current = true;
    setReason(null);
  }, []);

  return { reason, version: version ?? "", onStar, onDismiss };
}
