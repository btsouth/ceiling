import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LocaleProvider } from "../../../../i18n/LocaleProvider";
import { buildBundle } from "../../../../test/localeHarness";
import type { ProviderDetail } from "../../../../types/bridge";
import { QuickActionsSection } from "./QuickActionsSection";

const tauriMocks = vi.hoisted(() => ({
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  openExternalUrl: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
}));

vi.mock("../../../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../../lib/tauri")>()),
  ...tauriMocks,
}));
vi.mock("@tauri-apps/api/event", () => eventMocks);

function provider(): ProviderDetail {
  return {
    id: "copilot",
    displayName: "GitHub Copilot",
    enabled: true,
    email: null,
    plan: null,
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
    supportsLogin: false,
    hasSnapshot: false,
    cookieSource: null,
    region: null,
  };
}

const noop = vi.fn();

describe("QuickActionsSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.getLocaleStrings.mockResolvedValue(buildBundle());
    tauriMocks.openExternalUrl.mockResolvedValue(undefined);
    eventMocks.listen.mockResolvedValue(() => {});
  });

  function renderActions(detail: ProviderDetail) {
    return render(
      <QuickActionsSection
        provider={detail}
        busy={false}
        loginPhase={null}
        loginCode={null}
        loginUrl={null}
        onRefresh={noop}
        onSwitchAccount={noop}
        onOpenDashboard={noop}
        onOpenStatusPage={noop}
        onCopyError={noop}
        onBuyCredits={noop}
        t={(key) => key}
      />,
    );
  }

  it("offers Switch account for providers with in-app login, even without a dashboard", () => {
    renderActions({ ...provider(), supportsLogin: true, dashboardUrl: null });
    expect(
      screen.getByRole("button", { name: "ActionSwitchAccount" }),
    ).toBeInTheDocument();
  });

  it("hides Switch account for dashboard-only providers that cannot sign in from the app", () => {
    renderActions({
      ...provider(),
      id: "cursor",
      supportsLogin: false,
      dashboardUrl: "https://cursor.com/dashboard",
    });
    expect(
      screen.queryByRole("button", { name: "ActionSwitchAccount" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "ActionUsageDashboard" }),
    ).toBeInTheDocument();
  });

  it("shows the device-flow code and verification link alongside the login status", async () => {
    render(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="waitingBrowser"
          loginCode="ABCD-1234"
          loginUrl="https://github.com/login/device"
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    expect(await screen.findByRole("status")).toHaveTextContent(
      "LoginPhaseWaitingBrowser LoginPhaseEnterGithubCodePrefix ABCD-1234 LoginPhaseOpenVerificationLink",
    );

    fireEvent.click(
      screen.getByRole("button", { name: "LoginPhaseOpenVerificationLink" }),
    );
    expect(tauriMocks.openExternalUrl).toHaveBeenCalledWith(
      "https://github.com/login/device",
    );
  });

  it("shows an inline error when opening the verification link fails", async () => {
    tauriMocks.openExternalUrl.mockRejectedValue(new Error("no browser"));

    render(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="waitingBrowser"
          loginCode="ABCD-1234"
          loginUrl="https://github.com/login/device"
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "LoginPhaseOpenVerificationLink",
      }),
    );

    expect(await screen.findByText("no browser")).toBeInTheDocument();
  });

  it("clears a stale link error once a new login attempt starts", async () => {
    tauriMocks.openExternalUrl.mockRejectedValue(new Error("no browser"));

    const { rerender } = render(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="waitingBrowser"
          loginCode="ABCD-1234"
          loginUrl="https://github.com/login/device"
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "LoginPhaseOpenVerificationLink",
      }),
    );
    expect(await screen.findByText("no browser")).toBeInTheDocument();

    // Same URL as before — GitHub's plain verification_uri is a constant,
    // so the clear must be keyed on the code, not the URL, to catch this.
    rerender(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="waitingBrowser"
          loginCode="WXYZ-5678"
          loginUrl="https://github.com/login/device"
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    expect(screen.queryByText("no browser")).not.toBeInTheDocument();
  });

  it("does not render a code or link during waitingBrowser when none is carried yet", async () => {
    render(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="waitingBrowser"
          loginCode={null}
          loginUrl={null}
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    expect(await screen.findByRole("status")).not.toHaveTextContent(
      "LoginPhaseEnterGithubCodePrefix",
    );
  });

  it("does not render a stale code or link once the phase moves past waitingBrowser", async () => {
    render(
      <LocaleProvider>
        <QuickActionsSection
          provider={provider()}
          busy={false}
          loginPhase="complete"
          loginCode="ABCD-1234"
          loginUrl="https://github.com/login/device"
          onRefresh={noop}
          onSwitchAccount={noop}
          onOpenDashboard={noop}
          onOpenStatusPage={noop}
          onCopyError={noop}
          onBuyCredits={noop}
          t={(key) => key}
        />
      </LocaleProvider>,
    );

    await screen.findByRole("status");
    expect(screen.queryByText("ABCD-1234")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "LoginPhaseOpenVerificationLink" }),
    ).not.toBeInTheDocument();
  });
});
