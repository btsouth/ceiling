import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ProviderDetail } from "../../../../types/bridge";
import { IdentitySection } from "./IdentitySection";

vi.mock("../../../../components/providers/ProviderIcon", () => ({
  ProviderIcon: () => null,
}));

function provider(): ProviderDetail {
  return {
    id: "codex",
    displayName: "Codex",
    enabled: true,
    accountId: null,
    accounts: [],
    email: "person@example.com",
    plan: "Pro",
    authType: null,
    sourceLabel: null,
    organization: null,
    lastUpdated: null,
    session: null,
    weekly: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    pace: null,
    lastError: null,
    dashboardUrl: null,
    statusPageUrl: null,
    buyCreditsUrl: null,
    supportsLogin: true,
    hasSnapshot: true,
    cookieSource: null,
    region: null,
  };
}

describe("IdentitySection", () => {
  it("shows the account email when Hide personal info is off", () => {
    render(<IdentitySection provider={provider()} subtitle="" t={(key) => key} />);
    expect(screen.getByText("person@example.com")).toBeInTheDocument();
  });

  it("masks the account email when Hide personal info is on", () => {
    render(
      <IdentitySection provider={provider()} subtitle="" hideEmail t={(key) => key} />,
    );
    expect(screen.queryByText("person@example.com")).not.toBeInTheDocument();
    expect(screen.getByText("p•••••@example.com")).toBeInTheDocument();
  });
});
