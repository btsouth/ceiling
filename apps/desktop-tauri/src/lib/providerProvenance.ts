import type { LocaleKey } from "../i18n/keys";

/**
 * Providers with hand-written, source-verified provenance copy for the
 * "How Ceiling gets this data" block (SOU-179). Every other provider falls
 * back to `DataSourceGeneric`.
 *
 * The copy behind these keys is verified against the real fetch paths in
 * `rust/src/providers/*` and must stay in sync with `docs/DATA_SOURCES.md`.
 * A test (`providerProvenance.test.ts`) asserts these five keep dedicated
 * copy so a rename can't silently drop a first-class provider to the generic
 * line.
 */
const PROVENANCE_KEYS: Record<string, LocaleKey> = {
  claude: "DataSourceClaude",
  codex: "DataSourceCodex",
  cursor: "DataSourceCursor",
  copilot: "DataSourceCopilot",
  gemini: "DataSourceGemini",
  grok: "DataSourceGrok",
  deepseekharness: "DataSourceDeepSeekHarness",
};

/** Provider ids that ship dedicated (non-generic) provenance copy. */
export const DETAILED_PROVENANCE_PROVIDERS = Object.keys(PROVENANCE_KEYS);

/**
 * Locale key describing how Ceiling obtains this provider's data. Providers
 * without dedicated copy get the precise generic statement.
 */
export function providerProvenanceKey(providerId: string): LocaleKey {
  return PROVENANCE_KEYS[providerId] ?? "DataSourceGeneric";
}
