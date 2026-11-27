import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
}));

vi.mock("../../../lib/tauri", () => ({
  ...tauriMocks,
  openProviderDashboard: vi.fn(),
  openProviderStatusPage: vi.fn(),
  refreshProviders: vi.fn(),
  revokeProviderCredentials: vi.fn(),
  setProviderGatewayUrl: vi.fn(),
  triggerProviderLogin: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

vi.mock("./sections/IdentitySection", () => ({ IdentitySection: () => null }));
vi.mock("./sections/DataSourceSection", () => ({ DataSourceSection: () => null }));
vi.mock("./sections/UsageSection", () => ({ UsageSection: () => null }));
vi.mock("./sections/PaceSection", () => ({ PaceSection: () => null }));
vi.mock("./sections/CostSection", () => ({ CostSection: () => null }));
vi.mock("./sections/QuickActionsSection", () => ({ QuickActionsSection: () => null }));
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
vi.mock("./CookieSection", () => ({ CookieSection: () => null }));
vi.mock("./sections/MenuBarMetricSection", () => ({ MenuBarMetricSection: () => null }));
vi.mock("./ApiKeySection", () => ({
  ApiKeySection: ({
    onCredentialsChanged,
  }: {
    onCredentialsChanged?: () => void;
  }) => <button onClick={onCredentialsChanged}>save-api-key</button>,
}));

const absentStatus: CredentialStorageStatus = {
  apiKeys: { fileStatus: "missing", hasProviderCredentials: false },
  manualCookies: { fileStatus: "missing", hasProviderCredentials: false },
  tokenAccounts: { fileStatus: "missing", hasProviderCredentials: false },
};

const apiKeyPresentStatus: CredentialStorageStatus = {
  ...absentStatus,
  apiKeys: {
    fileStatus: "protected:windows-dpapi-user",
    hasProviderCredentials: true,
  },
};

const providerDetail: ProviderDetail = {
  id: "claude",
  displayName: "Claude",
  enabled: true,
  accountId: null,
  accounts: [],
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
  hasSnapshot: true,
  cookieSource: null,
  region: null,
};

describe("ProviderDetailPane credential status refresh", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.getProviderDetail.mockResolvedValue(providerDetail);
    tauriMocks.getProviderRegionOptions.mockResolvedValue([]);
    tauriMocks.getTokenAccountProviders.mockResolvedValue([]);
  });

  it("refreshes storage labels and exposes revoke after an inline save", async () => {
    tauriMocks.getCredentialStorageStatus
      .mockResolvedValueOnce(absentStatus)
      .mockResolvedValueOnce(apiKeyPresentStatus);

    render(
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
      expect(screen.getAllByText("CredentialStatusNotCreated")).toHaveLength(3),
    );
    expect(
      screen.queryByRole("button", { name: "CredentialRevokeStored" }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "save-api-key" }));

    expect(
      await screen.findByText(
        "CredentialProtectedPrefix (windows-dpapi-user)",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "CredentialRevokeStored" }),
    ).toBeInTheDocument();
  });

  it("does not leak a Claude account tab into the next provider's token status", async () => {
    const claudeWorkId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    const claudePersonalId = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    const claudeDetail: ProviderDetail = {
      ...providerDetail,
      accountId: claudeWorkId,
      accounts: [
        { accountId: claudeWorkId, label: "Work", tint: null },
        { accountId: claudePersonalId, label: "Personal", tint: null },
      ],
    };
    const cursorDetail: ProviderDetail = {
      ...providerDetail,
      id: "cursor",
      displayName: "Cursor",
    };
    const tokenPresentStatus: CredentialStorageStatus = {
      ...absentStatus,
      tokenAccounts: {
        fileStatus: "protected:windows-dpapi-user",
        hasProviderCredentials: true,
      },
    };

    tauriMocks.getProviderDetail.mockImplementation(
      async (id: string, accountId: string | null) => {
        if (id === "cursor") return cursorDetail;
        if (accountId === claudePersonalId) {
          return { ...claudeDetail, accountId: claudePersonalId };
        }
        return claudeDetail;
      },
    );
    tauriMocks.getCredentialStorageStatus.mockImplementation(async (id: string) =>
      id === "cursor" ? tokenPresentStatus : absentStatus,
    );

    const view = render(
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
      expect(screen.getByRole("tab", { name: "Personal" })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("tab", { name: "Personal" }));
    await waitFor(() =>
      expect(tauriMocks.getProviderDetail).toHaveBeenCalledWith(
        "claude",
        claudePersonalId,
      ),
    );

    view.rerender(
      <ProviderDetailPane
        providerId="cursor"
        resetTimeRelative
        providerMetrics={{}}
        wayfinderGatewayUrl=""
        settingsDisabled={false}
        onSettingsChange={vi.fn()}
      />,
    );

    expect(
      await screen.findByText(
        "CredentialProtectedPrefix (windows-dpapi-user)",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "CredentialRevokeStored" }),
    ).toBeInTheDocument();
    expect(tauriMocks.getProviderDetail).toHaveBeenCalledWith("cursor", null);
    expect(tauriMocks.getProviderDetail).not.toHaveBeenCalledWith(
      "cursor",
      claudePersonalId,
    );
    expect(tauriMocks.getCredentialStorageStatus).toHaveBeenCalledWith("cursor");
  });
});
