import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getAppInfo: vi.fn(),
  openExternalUrl: vi.fn(),
}));

const localeStrings = vi.hoisted(() => ({
  current: {
    AboutLinkErrorPrefix: "Error:",
  } as Record<string, string>,
}));

const idleUpdateState = {
  status: "idle" as const,
  version: null,
  error: null,
  progress: null,
  releaseUrl: null,
  canDownload: false,
  canApply: false,
  lastCheckedAt: null,
};

const updateMocks = vi.hoisted(() => ({
  checkNow: vi.fn(),
  download: vi.fn(),
  apply: vi.fn(),
  dismiss: vi.fn(),
  openRelease: vi.fn(),
  updateState: {
    status: "idle" as const,
    version: null as string | null,
    error: null as string | null,
    progress: null as number | null,
    releaseUrl: null as string | null,
    canDownload: false,
    canApply: false,
    lastCheckedAt: null as number | null,
  },
}));

vi.mock("../../../lib/tauri", () => tauriMocks);
vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({
    t: (key: string) => localeStrings.current[key] ?? key,
  }),
}));
vi.mock("../../../hooks/useUpdateState", () => ({
  useUpdateState: () => ({
    updateState: updateMocks.updateState,
    checkNow: updateMocks.checkNow,
    download: updateMocks.download,
    apply: updateMocks.apply,
    dismiss: updateMocks.dismiss,
    openRelease: updateMocks.openRelease,
  }),
}));

import AboutTab from "./AboutTab";
import type { SettingsSnapshot } from "../../../types/bridge";

const settings: SettingsSnapshot = {
  enabledProviders: [],
  refreshIntervalSecs: 300,
  refreshAllProvidersOnMenuOpen: false,
  startAtLogin: false,
  startMinimized: false,
  showNotifications: true,
  capacityEventNotificationsEnabled: true,
  soundEnabled: true,
  soundVolume: 100,
  highUsageThreshold: 70,
  criticalUsageThreshold: 90,
  predictivePaceWarningEnabled: false,
  switcherShowsIcons: true,
  menuBarShowsHighestUsage: true,
  menuBarShowsPercent: true,
  showAsUsed: false,
  showAllTokenAccountsInMenu: true,
  enableAnimations: true,
  resetTimeRelative: true,
  showResetWhenExhausted: false,
  menuBarDisplayMode: "compact",
  hidePersonalInfo: false,
  autoDownloadUpdates: false,
  installUpdatesOnQuit: false,
  globalShortcut: "",
  codexCustomSessionsDirs: [],
  updateChannel: "stable",
  uiLanguage: "english",
  theme: "dark",
  windowScalePercent: 125,
  trayScalePercent: 100,
  powertoysStatusPipeEnabled: false,
  claudeAvoidKeychainPrompts: true,
  codexSparkUsageVisible: true,
  disableKeychainAccess: false,
  providerMetrics: {},
  floatBarEnabled: false,
  taskbarWidgetEnabled: true,
  taskbarWidgetAllMonitors: false,
  floatBarOpacity: 0.9,
  floatBarScale: 100,
  floatBarOrientation: "horizontal",
  floatBarStyle: "floating",
  taskbarWidgetOpenOnHover: true,
  floatBarDensity: "standard",
  floatBarInformationMode: "exact",
  floatBarSelectionMode: "pinned",
  floatBarForegroundDetection: true,
  floatBarContrast: "auto",
  floatBarClickThrough: false,
  floatBarProviderIds: [],
    taskbarAccountByProvider: {},
  floatBarDarkText: false,
  floatBarShowResetInline: false,
  floatBarShowCost: false,
};

describe("AboutTab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localeStrings.current = {
      AboutLinkErrorPrefix: "Error:",
      AboutUpdateCheckFailed: "Could not check for updates.",
      AboutUpToDate: "You're up to date!",
      AboutCheckForUpdates: "Check for Updates…",
    };
    Object.assign(updateMocks.updateState, idleUpdateState);
    tauriMocks.getAppInfo.mockResolvedValue({
      name: "Ceiling",
      version: "0.30.3",
      buildNumber: "dev",
      updateChannel: "stable",
      tagline: "Keep agent limits in view.",
    });
    tauriMocks.openExternalUrl.mockResolvedValue(undefined);
  });

  it("opens about links through the Tauri URL bridge", async () => {
    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);

    fireEvent.click(await screen.findByRole("button", { name: "GitHub" }));
    fireEvent.click(screen.getByRole("button", { name: "Website" }));
    fireEvent.click(screen.getByRole("button", { name: "Win-CodexBar" }));
    fireEvent.click(screen.getByRole("button", { name: "CodexBar" }));

    expect(tauriMocks.openExternalUrl).toHaveBeenNthCalledWith(
      1,
      "https://github.com/btsouth/ceiling",
    );
    expect(tauriMocks.openExternalUrl).toHaveBeenNthCalledWith(
      2,
      "https://ceiling.win",
    );
    expect(tauriMocks.openExternalUrl).toHaveBeenNthCalledWith(
      3,
      "https://github.com/Finesssee/Win-CodexBar",
    );
    expect(tauriMocks.openExternalUrl).toHaveBeenNthCalledWith(
      4,
      "https://github.com/steipete/CodexBar",
    );
  });

  it("keeps update controls simple and credits both upstream projects", async () => {
    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);

    await screen.findByText("Ceiling");
    expect(screen.queryByText("UpdateChannelChoice")).not.toBeInTheDocument();
    expect(screen.queryByText("UpdateChannelStableOption")).not.toBeInTheDocument();
    expect(screen.queryByText("UpdateChannelBetaOption")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Win-CodexBar" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "CodexBar" })).toBeInTheDocument();
  });

  it("does not leave English copyright fragments when those keys are Chinese", async () => {
    localeStrings.current = {
      AboutLinkErrorPrefix: "Error:",
      AboutCopyrightPrefix: "Ceiling · MIT 许可证 · 派生自",
      AboutCopyrightMid: "，基于",
      AboutCopyrightSuffix: "，作者 Peter Steinberger。",
    };

    const { container } = render(
      <AboutTab settings={settings} set={vi.fn()} saving={false} />,
    );

    await screen.findByText("Ceiling");
    expect(container.textContent).not.toContain("which is based on");
    expect(container.textContent).not.toContain("by Peter Steinberger");
    expect(container.textContent).toContain("派生自");
    expect(container.textContent).toContain("基于");
    expect(container.textContent).toContain("CodexBar，");
    expect(container.textContent).not.toContain("CodexBar ，");
  });

  it("shows a link error if the OS browser launch fails", async () => {
    tauriMocks.openExternalUrl.mockRejectedValue("no browser");

    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);

    fireEvent.click(await screen.findByRole("button", { name: "Website" }));

    await waitFor(() => {
      expect(screen.getByText("Error: no browser")).toBeInTheDocument();
    });
  });

  it("shows up to date only after a successful idle check", async () => {
    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Check for Updates…" }));
    expect(screen.getByText("You're up to date!")).toBeInTheDocument();
    expect(screen.queryByText(/Could not check for updates/)).not.toBeInTheDocument();
  });

  it("does not claim the user is current when the update check failed", async () => {
    Object.assign(updateMocks.updateState, {
      status: "error",
      error: "GitHub did not return a release.",
    });

    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Check for Updates…" }));

    expect(
      screen.getByText("GitHub did not return a release."),
    ).toBeInTheDocument();
    expect(screen.queryByText("You're up to date!")).not.toBeInTheDocument();
  });

  it("does not label a download failure as a failed update check", async () => {
    // Download and Install set their own errors. Prefixing them with the
    // check-failure sentence told the user the wrong thing went wrong.
    Object.assign(updateMocks.updateState, {
      status: "error",
      error: "SHA256 mismatch: the installer did not match its published hash.",
    });

    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Check for Updates…" }));

    expect(
      screen.getByText("SHA256 mismatch: the installer did not match its published hash."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Could not check for updates/)).not.toBeInTheDocument();
  });

  it("falls back to the check-failure sentence when the error is empty", async () => {
    Object.assign(updateMocks.updateState, { status: "error", error: null });

    render(<AboutTab settings={settings} set={vi.fn()} saving={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "Check for Updates…" }));

    expect(screen.getByText("Could not check for updates.")).toBeInTheDocument();
  });
});
