import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  CredentialStorageStatus,
  ProviderDetail,
} from "../../../types/bridge";
import { ProviderDetailPane } from "./ProviderDetailPane";

const tauriMocks = vi.hoisted(() => ({
  getCredentialStorageStatus: vi.fn(),
  getProviderDetail: vi.fn(),
  getProviderRegionOptions: vi.fn(),
  getTokenAccountProviders: vi.fn(),
  openProviderDashboard: vi.fn(),
  openProviderStatusPage: vi.fn(),
  refreshProviders: vi.fn(),
  revokeProviderCredentials: vi.fn(),
  setProviderGatewayUrl: vi.fn(),
  triggerProviderLogin: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  handlers: new Map<string, Set<(event: { payload: unknown }) => void>>(),
  listen: vi.fn(),
}));

vi.mock("../../../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => ({ listen: eventMocks.listen }));
vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

vi.mock("./sections/IdentitySection", () => ({
  IdentitySection: ({ provider }: { provider: ProviderDetail }) => (
    <div data-testid="provider-identity">
      {provider.id}:{provider.accountId ?? "ambient"}:{provider.email ?? "none"}
    </div>
  ),
}));

vi.mock("./sections/DataSourceSection", () => ({ DataSourceSection: () => null }));
vi.mock("./sections/UsageSection", () => ({ UsageSection: () => null }));
vi.mock("./sections/PaceSection", () => ({ PaceSection: () => null }));
vi.mock("./sections/CostSection", () => ({ CostSection: () => null }));
vi.mock("./sections/QuickActionsSection", () => ({
  QuickActionsSection: ({
    onSwitchAccount,
  }: {
    onSwitchAccount: () => void;
  }) => <button onClick={onSwitchAccount}>switch-account</button>,
}));
vi.mock("./sections/charts/ChartsSection", () => ({ ChartsSection: () => null }));
vi.mock("./sections/RegionSection", () => ({ RegionSection: () => null }));
vi.mock("./sections/credentials/GeminiCliCreds", () => ({ GeminiCliCreds: () => null }));
vi.mock("./sections/credentials/VertexAiCreds", () => ({ VertexAiCreds: () => null }));
vi.mock("./sections/credentials/JetBrainsCreds", () => ({ JetBrainsCreds: () => null }));
vi.mock("./sections/credentials/KiroCreds", () => ({ KiroCreds: () => null }));
vi.mock("./sections/credentials/ClaudeCreds", () => ({ ClaudeCreds: () => null }));
vi.mock("./sections/credentials/CodexUsageOptions", () => ({ CodexUsageOptions: () => null }));
vi.mock("./sections/credentials/OpenAiExtras", () => ({ OpenAiExtras: () => null }));
vi.mock("../tokens/TokenAccountsPanel", () => ({ TokenAccountsPanel: () => null }));
vi.mock("./ApiKeySection", () => ({ ApiKeySection: () => null }));
vi.mock("./CookieSection", () => ({ CookieSection: () => null }));
vi.mock("./sections/MenuBarMetricSection", () => ({ MenuBarMetricSection: () => null }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function detail(
  providerId: string,
  accountId: string | null,
  email: string,
): ProviderDetail {
  return {
    id: providerId,
    displayName: providerId === "codex" ? "Codex" : "Claude",
    enabled: true,
    accountId,
    accounts:
      providerId === "codex"
        ? [
            { accountId: "personal", label: "Personal", tint: null },
            { accountId: "work", label: "Work", tint: null },
          ]
        : [],
    email,
    plan: null,
    authType: null,
    sourceLabel: "oauth",
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
    hasSnapshot: true,
    cookieSource: null,
    region: null,
  };
}

function renderPane(
  providerId: string | null,
  options: { hidePersonalInfo?: boolean } = {},
) {
  return render(
    <ProviderDetailPane
      providerId={providerId}
      hidePersonalInfo={options.hidePersonalInfo}
      resetTimeRelative
      providerMetrics={{}}
      wayfinderGatewayUrl=""
      settingsDisabled={false}
      onSettingsChange={vi.fn()}
    />,
  );
}

const absentStatus: CredentialStorageStatus = {
  apiKeys: { fileStatus: "missing", hasProviderCredentials: false },
  manualCookies: { fileStatus: "missing", hasProviderCredentials: false },
  tokenAccounts: { fileStatus: "missing", hasProviderCredentials: false },
};

const presentStatus: CredentialStorageStatus = {
  ...absentStatus,
  apiKeys: {
    fileStatus: "protected:windows-dpapi-user",
    hasProviderCredentials: true,
  },
};

function emitProviderUpdated(providerId: string) {
  for (const handler of eventMocks.handlers.get("provider-updated") ?? []) {
    handler({ payload: { providerId } });
  }
}

describe("ProviderDetailPane request ordering", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.getProviderDetail.mockReset();
    tauriMocks.getCredentialStorageStatus.mockReset();
    tauriMocks.triggerProviderLogin.mockReset();
    tauriMocks.revokeProviderCredentials.mockReset();
    eventMocks.handlers.clear();
    eventMocks.listen.mockImplementation(
      async (eventName: string, handler: (event: { payload: unknown }) => void) => {
        const handlers = eventMocks.handlers.get(eventName) ?? new Set();
        handlers.add(handler);
        eventMocks.handlers.set(eventName, handlers);
        return () => handlers.delete(handler);
      },
    );
    tauriMocks.getCredentialStorageStatus.mockResolvedValue(absentStatus);
    tauriMocks.getProviderRegionOptions.mockResolvedValue([]);
    tauriMocks.getTokenAccountProviders.mockResolvedValue([]);
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.revokeProviderCredentials.mockResolvedValue(undefined);
    tauriMocks.triggerProviderLogin.mockResolvedValue(undefined);
  });

  it("shows an action error as an alert while the detail stays loaded, and clears it on provider change", async () => {
    tauriMocks.getProviderDetail.mockImplementation((providerId: string) =>
      Promise.resolve(detail(providerId, null, `${providerId}@example.com`)),
    );
    tauriMocks.triggerProviderLogin.mockRejectedValue("login exploded");

    const view = renderPane("claude");
    await screen.findByTestId("provider-identity");

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "switch-account" }));
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("login exploded");
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "claude:ambient:claude@example.com",
    );

    view.rerender(
      <ProviderDetailPane
        providerId="codex"
        resetTimeRelative
        providerMetrics={{}}
        wayfinderGatewayUrl=""
        settingsDisabled={false}
        onSettingsChange={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.getByTestId("provider-identity")).toHaveTextContent("codex:"),
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("masks email account tab labels when Hide personal info is on", async () => {
    const withEmailTabs: ProviderDetail = {
      ...detail("codex", "personal", "personal@example.com"),
      accounts: [
        { accountId: "personal", label: "personal@example.com", tint: null },
        { accountId: "work", label: "Work", tint: null },
      ],
    };
    tauriMocks.getProviderDetail.mockResolvedValue(withEmailTabs);

    renderPane("codex", { hidePersonalInfo: true });
    await screen.findByTestId("provider-identity");

    expect(screen.queryByText("personal@example.com")).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "p•••••••@example.com" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Work" })).toBeInTheDocument();
  });

  it("keeps the newest account when account requests resolve out of order", async () => {
    const work = deferred<ProviderDetail>();
    const personal = deferred<ProviderDetail>();
    tauriMocks.getProviderDetail.mockImplementation(
      (_providerId: string, accountId: string | null) => {
        if (accountId === "work") return work.promise;
        if (accountId === "personal") return personal.promise;
        return Promise.resolve(detail("codex", "personal", "personal@example.com"));
      },
    );

    renderPane("codex");
    expect(await screen.findByTestId("provider-identity")).toHaveTextContent(
      "codex:personal:personal@example.com",
    );

    act(() => {
      fireEvent.click(screen.getByRole("tab", { name: "Work" }));
      fireEvent.click(screen.getByRole("tab", { name: "Personal" }));
    });

    await act(async () => {
      personal.resolve(detail("codex", "personal", "new-personal@example.com"));
    });
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "codex:personal:new-personal@example.com",
    );

    await act(async () => {
      work.resolve(detail("codex", "work", "work@example.com"));
    });
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "codex:personal:new-personal@example.com",
    );
  });

  it("keeps the newest provider when provider requests resolve out of order", async () => {
    const codex = deferred<ProviderDetail>();
    const claude = deferred<ProviderDetail>();
    tauriMocks.getProviderDetail.mockImplementation((providerId: string) =>
      providerId === "codex" ? codex.promise : claude.promise,
    );

    const view = renderPane("codex");
    await waitFor(() =>
      expect(tauriMocks.getProviderDetail).toHaveBeenCalledWith("codex", null),
    );
    view.rerender(
      <ProviderDetailPane
        providerId="claude"
        resetTimeRelative
        providerMetrics={{}}
        wayfinderGatewayUrl=""
        settingsDisabled={false}
        onSettingsChange={vi.fn()}
      />,
    );

    await act(async () => {
      claude.resolve(detail("claude", null, "claude@example.com"));
    });
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "claude:ambient:claude@example.com",
    );

    await act(async () => {
      codex.resolve(detail("codex", null, "codex@example.com"));
    });
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "claude:ambient:claude@example.com",
    );
  });

  it("coalesces a burst of provider-updated events", async () => {
    tauriMocks.getProviderDetail.mockResolvedValue(
      detail("codex", "personal", "personal@example.com"),
    );
    renderPane("codex");
    await screen.findByTestId("provider-identity");

    act(() => {
      emitProviderUpdated("codex");
      emitProviderUpdated("codex");
      emitProviderUpdated("codex");
    });
    await waitFor(() =>
      expect(tauriMocks.getProviderDetail).toHaveBeenCalledTimes(2),
    );
  });

  it("does not let a completed switch-account flow clear a newer provider selection", async () => {
    const login = deferred<void>();
    tauriMocks.triggerProviderLogin.mockReturnValue(login.promise);
    tauriMocks.getProviderDetail.mockImplementation(
      (providerId: string, accountId: string | null) =>
        Promise.resolve({
          ...detail(
            providerId,
            accountId ?? "personal",
            `${accountId ?? "personal"}@${providerId}.example.com`,
          ),
          accounts: [
            { accountId: "personal", label: "Personal", tint: null },
            { accountId: "work", label: "Work", tint: null },
          ],
        }),
    );

    const view = renderPane("codex");
    await screen.findByTestId("provider-identity");
    fireEvent.click(screen.getByRole("button", { name: "switch-account" }));

    view.rerender(
      <ProviderDetailPane
        providerId="claude"
        resetTimeRelative
        providerMetrics={{}}
        wayfinderGatewayUrl=""
        settingsDisabled={false}
        onSettingsChange={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.getByTestId("provider-identity")).toHaveTextContent(
        "claude:personal:personal@claude.example.com",
      ),
    );
    fireEvent.click(screen.getByRole("tab", { name: "Work" }));
    await waitFor(() =>
      expect(screen.getByTestId("provider-identity")).toHaveTextContent(
        "claude:work:work@claude.example.com",
      ),
    );

    await act(async () => login.resolve());
    await waitFor(() => expect(tauriMocks.refreshProviders).toHaveBeenCalled());
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "claude:work:work@claude.example.com",
    );
  });

  it("falls back to ambient detail when the selected account fails", async () => {
    tauriMocks.getProviderDetail.mockImplementation(
      (providerId: string, accountId: string | null) => {
        if (accountId === "work") return Promise.reject(new Error("missing account"));
        return Promise.resolve(detail(providerId, "personal", "personal@example.com"));
      },
    );

    renderPane("codex");
    await screen.findByTestId("provider-identity");
    fireEvent.click(screen.getByRole("tab", { name: "Work" }));

    await waitFor(() =>
      expect(tauriMocks.getProviderDetail).toHaveBeenLastCalledWith("codex", null),
    );
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "codex:personal:personal@example.com",
    );
    expect(screen.getByRole("tab", { name: "Personal" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("cancels a queued provider refresh when an account is selected manually", async () => {
    tauriMocks.getProviderDetail.mockImplementation(
      (providerId: string, accountId: string | null) =>
        Promise.resolve(
          detail(
            providerId,
            accountId ?? "personal",
            `${accountId ?? "personal"}@example.com`,
          ),
        ),
    );

    renderPane("codex");
    await screen.findByTestId("provider-identity");
    act(() => {
      emitProviderUpdated("codex");
      fireEvent.click(screen.getByRole("tab", { name: "Work" }));
    });
    await waitFor(() =>
      expect(screen.getByTestId("provider-identity")).toHaveTextContent(
        "codex:work:work@example.com",
      ),
    );
    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 75));
    });
    expect(tauriMocks.getProviderDetail).toHaveBeenCalledTimes(2);
  });

  it("returns to ambient detail after revoking credentials", async () => {
    tauriMocks.getProviderDetail.mockImplementation(
      (providerId: string, accountId: string | null) =>
        Promise.resolve(
          detail(
            providerId,
            accountId ?? "personal",
            `${accountId ?? "personal"}@example.com`,
          ),
        ),
    );

    tauriMocks.getCredentialStorageStatus.mockResolvedValue(presentStatus);
    renderPane("codex");
    await screen.findByTestId("provider-identity");
    fireEvent.click(screen.getByRole("tab", { name: "Work" }));
    await waitFor(() =>
      expect(screen.getByTestId("provider-identity")).toHaveTextContent(
        "codex:work:work@example.com",
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: "CredentialRevokeStored" }));
    expect(tauriMocks.revokeProviderCredentials).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "ConfirmRevoke" }));
    await waitFor(() =>
      expect(tauriMocks.revokeProviderCredentials).toHaveBeenCalledWith("codex"),
    );
    await waitFor(() =>
      expect(tauriMocks.refreshProviders).toHaveBeenCalled(),
    );
    await waitFor(() =>
      expect(tauriMocks.getProviderDetail).toHaveBeenLastCalledWith("codex", null),
    );
    expect(screen.getByTestId("provider-identity")).toHaveTextContent(
      "codex:personal:personal@example.com",
    );
  });

  it("closes the revoke dialog even when the provider refresh fails", async () => {
    tauriMocks.getProviderDetail.mockImplementation(
      (providerId: string, accountId: string | null) =>
        Promise.resolve(
          detail(
            providerId,
            accountId ?? "personal",
            `${accountId ?? "personal"}@example.com`,
          ),
        ),
    );
    tauriMocks.getCredentialStorageStatus
      .mockResolvedValueOnce(presentStatus)
      .mockResolvedValue(absentStatus);
    tauriMocks.refreshProviders.mockRejectedValueOnce(new Error("refresh failed"));

    renderPane("codex");
    await screen.findByTestId("provider-identity");
    fireEvent.click(screen.getByRole("button", { name: "CredentialRevokeStored" }));
    fireEvent.click(screen.getByRole("button", { name: "ConfirmRevoke" }));

    await waitFor(() =>
      expect(tauriMocks.revokeProviderCredentials).toHaveBeenCalledWith("codex"),
    );
    expect(await screen.findByRole("status")).toHaveTextContent("CredentialRevoked");
    expect(screen.queryByRole("alertdialog")).toBeNull();
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "CredentialRevokeStored" }),
      ).toBeNull(),
    );
  });
});
