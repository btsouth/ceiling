import { describe, expect, it } from "vitest";
import { ALL_LOCALE_KEYS } from "../i18n/keys";
import {
  DETAILED_PROVENANCE_PROVIDERS,
  providerProvenanceKey,
} from "./providerProvenance";

describe("providerProvenance", () => {
  it("gives the five first-class providers dedicated (non-generic) copy", () => {
    expect([...DETAILED_PROVENANCE_PROVIDERS].sort()).toEqual(
      ["claude", "codex", "copilot", "cursor", "deepseekharness", "gemini", "grok"].sort(),
    );
    for (const id of DETAILED_PROVENANCE_PROVIDERS) {
      expect(providerProvenanceKey(id)).not.toBe("DataSourceGeneric");
    }
  });

  it("falls back to the generic line for providers without dedicated copy", () => {
    expect(providerProvenanceKey("openrouter")).toBe("DataSourceGeneric");
    expect(providerProvenanceKey("brand-new-provider")).toBe("DataSourceGeneric");
  });

  // Guardrail (SOU-179): every provenance key a provider can resolve to must
  // exist in the locale set, so a provider can never render a missing string.
  it("every provenance key exists in the locale key set", () => {
    const keys = [
      ...DETAILED_PROVENANCE_PROVIDERS.map(providerProvenanceKey),
      providerProvenanceKey("unknown"),
    ];
    for (const key of keys) {
      expect(ALL_LOCALE_KEYS).toContain(key);
    }
  });
});
