import { invoke } from "@tauri-apps/api/core";
import type {
  ApiKeyInfoBridge,
  ApiKeyProviderInfoBridge,
  AppInfoBridge,
  BootstrapState,
  CurrentSurfaceState,
  ProofCommand,
  ProofStatePayload,
  CookieInfoBridge,
  DetectedProviderAccount,
  Language,
  LocaleStrings,
  ProviderCatalogEntry,
  ProviderChartData,
  ProviderDetail,
  ProviderLocalUsageSummary,
  LocalApiValueProvider,
  ActivityHeatmap,
  CursorActivitySnapshot,
  ProviderIncident,
  ProviderSummary,
  ProviderUsageSnapshot,
  AccountProbeBridge,
  ProviderAccountsBridge,
  ProviderTokenAccountsBridge,
  TokenAccountSupportBridge,
  SettingsSnapshot,
  SettingsTabId,
  SettingsUpdate,
  SurfaceMode,
  SurfaceTargetForMode,
  VisibleSurfaceMode,
  UpdateStatePayload,
  RegionOption,
  SafeDiagnostics,
  CredentialStorageStatus,
  TaskbarWidgetStatus,
  ForegroundProviderSnapshot,
  WorkAreaRect,
  AgentSession,
  AgentSessionDiscoveryResult,
  SessionFocusResult,
} from "../types/bridge";

export function getBootstrapState(): Promise<BootstrapState> {
  return invoke<BootstrapState>("get_bootstrap_state");
}

export function getDetectedProviderAccounts(): Promise<DetectedProviderAccount[]> {
  return invoke<DetectedProviderAccount[]>("get_detected_provider_accounts");
}

export function getProviderCatalog(): Promise<ProviderCatalogEntry[]> {
  return invoke<ProviderCatalogEntry[]>("get_provider_catalog");
}

export function reorderProviders(ids: string[]): Promise<ProviderSummary[]> {
  return invoke<ProviderSummary[]>("reorder_providers", { ids });
}

export function getSettingsSnapshot(): Promise<SettingsSnapshot> {
  return invoke<SettingsSnapshot>("get_settings_snapshot");
}

export function getForegroundProvider(): Promise<ForegroundProviderSnapshot> {
  return invoke<ForegroundProviderSnapshot>("get_foreground_provider");
}

export function updateSettings(
  patch: SettingsUpdate,
): Promise<SettingsSnapshot> {
  return invoke<SettingsSnapshot>("update_settings", { patch });
}

export function listAgentSessions(): Promise<AgentSessionDiscoveryResult> {
  return invoke<AgentSessionDiscoveryResult>("list_agent_sessions");
}

export function focusAgentSession(
  session: AgentSession,
): Promise<SessionFocusResult> {
  return invoke<SessionFocusResult>("focus_agent_session", { session });
}

export function setSurfaceMode<M extends VisibleSurfaceMode>(
  mode: M,
  target: SurfaceTargetForMode<M>,
): Promise<SurfaceMode> {
  return invoke<SurfaceMode>("set_surface_mode", { mode, target });
}

export function dismissTrayPanel(): Promise<void> {
  return invoke<void>("dismiss_tray_panel");
}

/** Sample the current Windows taskbar material for the native glance flyout. */
export function getTaskbarSurfaceColor(): Promise<string | null> {
  return invoke<string | null>("get_taskbar_surface_color");
}

/** Current native taskbar widget visibility, and why it's hidden if so. */
export function getTaskbarWidgetStatus(): Promise<TaskbarWidgetStatus> {
  return invoke<TaskbarWidgetStatus>("get_taskbar_widget_status");
}

/** Hide the primary dashboard while Ceiling continues running in the tray. */
export function hideDashboardToTray(): Promise<void> {
  return invoke<void>("hide_dashboard_to_tray");
}

/** Suppress flyout blur-dismiss while a resize/drag gesture is in flight. */
export function beginFlyoutGesture(): Promise<void> {
  return invoke<void>("begin_flyout_gesture");
}
export function endFlyoutGesture(): Promise<void> {
  return invoke<void>("end_flyout_gesture");
}

export function openSettingsWindow(tab: SettingsTabId): Promise<void> {
  return invoke<void>("open_settings_window", { tab });
}

/** Open (or focus) the detached flyout ("Pop Out Dashboard") window. */
export function openFlyoutWindow(): Promise<void> {
  return invoke<void>("open_flyout_window");
}

export function closeSettingsWindow(): Promise<void> {
  return invoke<void>("close_settings_window");
}

export function getCurrentSurfaceMode(): Promise<SurfaceMode> {
  return invoke<SurfaceMode>("get_current_surface_mode");
}

export function getCurrentSurfaceState(): Promise<CurrentSurfaceState> {
  return invoke<CurrentSurfaceState>("get_current_surface_state");
}

export function getProofState(): Promise<ProofStatePayload> {
  return invoke<ProofStatePayload>("get_proof_state");
}

export function runProofCommand(command: ProofCommand): Promise<ProofStatePayload> {
  return invoke<ProofStatePayload>("run_proof_command", { command });
}

export function refreshProviders(): Promise<void> {
  return invoke<void>("refresh_providers");
}

export function refreshProvidersIfStale(): Promise<void> {
  return invoke<void>("refresh_providers_if_stale");
}

export function getCachedProviders(): Promise<ProviderUsageSnapshot[]> {
  return invoke<ProviderUsageSnapshot[]>("get_cached_providers");
}

export function getSafeDiagnostics(): Promise<SafeDiagnostics> {
  return invoke<SafeDiagnostics>("get_safe_diagnostics");
}

export function getWorkAreaRect(): Promise<WorkAreaRect> {
  return invoke<WorkAreaRect>("get_work_area_rect");
}

export function getCredentialStorageStatus(
  providerId: string,
): Promise<CredentialStorageStatus> {
  return invoke<CredentialStorageStatus>("get_credential_storage_status", {
    providerId,
  });
}

export function getUpdateState(): Promise<UpdateStatePayload> {
  return invoke<UpdateStatePayload>("get_update_state");
}

export function checkForUpdates(): Promise<UpdateStatePayload> {
  return invoke<UpdateStatePayload>("check_for_updates");
}

export function downloadUpdate(): Promise<UpdateStatePayload> {
  return invoke<UpdateStatePayload>("download_update");
}

export function applyUpdate(): Promise<void> {
  return invoke<void>("apply_update");
}

export function dismissUpdate(): Promise<UpdateStatePayload> {
  return invoke<UpdateStatePayload>("dismiss_update");
}

export function openReleasePage(): Promise<void> {
  return invoke<void>("open_release_page");
}

export function openExternalUrl(url: string): Promise<void> {
  return invoke<void>("open_external_url", { url });
}

// ── Credential store bridge ──────────────────────────────────────────

export function getApiKeys(): Promise<ApiKeyInfoBridge[]> {
  return invoke<ApiKeyInfoBridge[]>("get_api_keys");
}

export function getApiKeyProviders(): Promise<ApiKeyProviderInfoBridge[]> {
  return invoke<ApiKeyProviderInfoBridge[]>("get_api_key_providers");
}

export function setApiKey(
  providerId: string,
  apiKey: string,
  label?: string,
): Promise<ApiKeyInfoBridge[]> {
  return invoke<ApiKeyInfoBridge[]>("set_api_key", {
    providerId,
    apiKey,
    label: label ?? null,
  });
}

export function removeApiKey(providerId: string): Promise<ApiKeyInfoBridge[]> {
  return invoke<ApiKeyInfoBridge[]>("remove_api_key", { providerId });
}

export function getManualCookies(): Promise<CookieInfoBridge[]> {
  return invoke<CookieInfoBridge[]>("get_manual_cookies");
}

export function setManualCookie(
  providerId: string,
  cookieHeader: string,
): Promise<CookieInfoBridge[]> {
  return invoke<CookieInfoBridge[]>("set_manual_cookie", {
    providerId,
    cookieHeader,
  });
}

export function removeManualCookie(
  providerId: string,
): Promise<CookieInfoBridge[]> {
  return invoke<CookieInfoBridge[]>("remove_manual_cookie", { providerId });
}

export function getAppInfo(): Promise<AppInfoBridge> {
  return invoke<AppInfoBridge>("get_app_info");
}

export function getProviderChartData(
  providerId: string,
  accountEmail?: string,
  accountId?: string,
  usageWindows?: import("../types/bridge").LocalUsageWindowRequest[],
  sourceLabel?: string,
  accountOrganization?: string,
): Promise<ProviderChartData> {
  return invoke<ProviderChartData>("get_provider_chart_data", {
    providerId,
    accountEmail,
    accountId,
    usageWindows,
    sourceLabel,
    accountOrganization,
  });
}

export function getProviderLocalUsageSummary(
  providerId: string,
): Promise<ProviderLocalUsageSummary | null> {
  return invoke<ProviderLocalUsageSummary | null>("get_provider_local_usage_summary", { providerId });
}

/** Local API-value totals. Optional inclusive YYYY-MM-DD custom range fills `custom`. */
export function getLocalApiValueTotals(options?: {
  since?: string;
  until?: string;
}): Promise<LocalApiValueProvider[]> {
  const since = options?.since?.trim();
  const until = options?.until?.trim();
  // Only send the range when both sides are real dates. Passing null for both
  // on every call made custom loads easy to mis-handle; omit args for presets.
  if (since && until) {
    return invoke<LocalApiValueProvider[]>("get_local_api_value_totals", {
      since,
      until,
    });
  }
  return invoke<LocalApiValueProvider[]>("get_local_api_value_totals");
}

/** Providers currently reporting a status-page incident, keyed by provider id. */
export function getProviderIncidents(): Promise<Record<string, ProviderIncident>> {
  return invoke<Record<string, ProviderIncident>>("get_provider_incidents");
}

/** Local activity by calendar day and clock hour, for the heatmap card. */
export function getLocalActivityHeatmap(): Promise<ActivityHeatmap> {
  return invoke<ActivityHeatmap>("get_local_activity_heatmap");
}

export function getCursorModelActivity(): Promise<CursorActivitySnapshot> {
  return invoke<CursorActivitySnapshot>("get_cursor_model_activity");
}

/** Export the provider's 30-day spend to a CSV in Downloads; resolves to the saved path. */
export function exportCostCsv(providerId: string): Promise<string> {
  return invoke<string>("export_cost_csv", { providerId });
}

/** Latest quota-run efficiency cards per window (SOU-299). */
export function getQuotaRunEfficiency(
  providerId: string,
  accountEmail?: string | null,
  accountId?: string | null,
): Promise<import("../types/bridge").QuotaRunEfficiency[]> {
  return invoke("get_quota_run_efficiency", {
    providerId,
    accountEmail: accountEmail ?? null,
    accountId: accountId ?? null,
  });
}

// ── Token account bridge ─────────────────────────────────────────────

export function getDirectoryAccounts(): Promise<ProviderAccountsBridge[]> {
  return invoke<ProviderAccountsBridge[]>("get_directory_accounts");
}

export function probeAccountDirectory(
  providerId: string,
  configDir: string,
): Promise<AccountProbeBridge> {
  return invoke<AccountProbeBridge>("probe_account_directory", {
    providerId,
    configDir,
  });
}

export function addDirectoryAccount(
  providerId: string,
  configDir: string,
  label: string | null,
): Promise<ProviderAccountsBridge> {
  return invoke<ProviderAccountsBridge>("add_directory_account", {
    providerId,
    configDir,
    label,
  });
}

export function removeDirectoryAccount(
  providerId: string,
  accountId: string,
): Promise<ProviderAccountsBridge> {
  return invoke<ProviderAccountsBridge>("remove_directory_account", {
    providerId,
    accountId,
  });
}

export function setActiveDirectoryAccount(
  providerId: string,
  accountId: string,
): Promise<ProviderAccountsBridge> {
  return invoke<ProviderAccountsBridge>("set_active_directory_account", {
    providerId,
    accountId,
  });
}

export function updateDirectoryAccount(
  providerId: string,
  accountId: string,
  label: string | null,
  tint: string | null,
): Promise<ProviderAccountsBridge> {
  return invoke<ProviderAccountsBridge>("update_directory_account", {
    providerId,
    accountId,
    label,
    tint,
  });
}

export function getTokenAccountProviders(): Promise<TokenAccountSupportBridge[]> {
  return invoke<TokenAccountSupportBridge[]>("get_token_account_providers");
}

export function getTokenAccounts(
  providerId: string,
): Promise<ProviderTokenAccountsBridge> {
  return invoke<ProviderTokenAccountsBridge>("get_token_accounts", { providerId });
}

export function addTokenAccount(
  providerId: string,
  label: string,
  token: string,
): Promise<ProviderTokenAccountsBridge> {
  return invoke<ProviderTokenAccountsBridge>("add_token_account", {
    providerId,
    label,
    token,
  });
}

export function removeTokenAccount(
  providerId: string,
  accountId: string,
): Promise<ProviderTokenAccountsBridge> {
  return invoke<ProviderTokenAccountsBridge>("remove_token_account", {
    providerId,
    accountId,
  });
}

export function setActiveTokenAccount(
  providerId: string,
  accountId: string,
): Promise<ProviderTokenAccountsBridge> {
  return invoke<ProviderTokenAccountsBridge>("set_active_token_account", {
    providerId,
    accountId,
  });
}

// ── Phase 5 — i18n ────────────────────────────────────────────────────

export function getLocaleStrings(
  language?: Language | null,
): Promise<LocaleStrings> {
  return invoke<LocaleStrings>("get_locale_strings", {
    language: language ?? null,
  });
}

export function setUiLanguage(language: Language): Promise<void> {
  return invoke<void>("set_ui_language", { language });
}

// ── Phase 6b — provider detail pane ──────────────────────────────────

export function getProviderDetail(
  providerId: string,
  accountId: string | null = null,
): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("get_provider_detail", {
    providerId,
    accountId,
  });
}

export function openProviderDashboard(providerId: string): Promise<void> {
  return invoke<void>("open_provider_dashboard", { providerId });
}

export function openProviderStatusPage(providerId: string): Promise<void> {
  return invoke<void>("open_provider_status_page", { providerId });
}

export function triggerProviderLogin(providerId: string): Promise<void> {
  return invoke<void>("trigger_provider_login", { providerId });
}

export function revokeProviderCredentials(providerId: string): Promise<void> {
  return invoke<void>("revoke_provider_credentials", { providerId });
}

// ── Region picker ────────────────────────────────────────────────────

export function getProviderRegionOptions(providerId: string): Promise<RegionOption[]> {
  return invoke<RegionOption[]>("get_provider_region_options", { providerId });
}

export function setProviderRegion(providerId: string, region: string): Promise<void> {
  return invoke<void>("set_provider_region", { providerId, region });
}

export function getProviderWorkspaceId(providerId: string): Promise<string | null> {
  return invoke<string | null>("get_provider_workspace_id", { providerId });
}

export function setProviderWorkspaceId(
  providerId: string,
  workspaceId: string,
): Promise<void> {
  return invoke<void>("set_provider_workspace_id", { providerId, workspaceId });
}

export function getProviderGatewayUrl(providerId: string): Promise<string | null> {
  return invoke<string | null>("get_provider_gateway_url", { providerId });
}

export function setProviderGatewayUrl(
  providerId: string,
  gatewayUrl: string,
): Promise<void> {
  return invoke<void>("set_provider_gateway_url", { providerId, gatewayUrl });
}

// ── Phase 6d — credential detection ──────────────────────────────────

export function openPath(path: string): Promise<void> {
  return invoke<void>("open_path", { path });
}

export function getGeminiCliSignedIn(): Promise<
  import("../types/bridge").GeminiCliStatus
> {
  return invoke("get_gemini_cli_signed_in");
}

export function getVertexAiStatus(): Promise<
  import("../types/bridge").VertexAiStatus
> {
  return invoke("get_vertexai_status");
}

export function listJetbrainsDetectedIdes(): Promise<
  import("../types/bridge").JetbrainsIde[]
> {
  return invoke("list_jetbrains_detected_ides");
}

export function setJetbrainsIdePath(path: string): Promise<void> {
  return invoke<void>("set_jetbrains_ide_path", { path });
}

export function getKiroStatus(): Promise<
  import("../types/bridge").KiroStatus
> {
  return invoke("get_kiro_status");
}

// ── Global shortcut validation ───────────────────────────────────────

export function validateGlobalShortcut(accelerator: string): Promise<void> {
  return invoke<void>("validate_global_shortcut", { accelerator });
}

export function playNotificationSound(): Promise<void> {
  return invoke<void>("play_notification_sound");
}

/** Show a real toast so the user can confirm notifications are delivered. */
export function sendTestNotification(): Promise<void> {
  return invoke<void>("send_test_notification");
}

export function reanchorTrayPanel(): Promise<void> {
  return invoke<void>("reanchor_tray_panel");
}

export function revealTrayPanelWindow(): Promise<void> {
  return invoke<void>("reveal_tray_panel_window");
}

/** Release the main or Settings window after its frontend has painted. */
export function revealReadyWindow(): Promise<void> {
  return invoke<void>("reveal_ready_window");
}

/** Persist the user's manually-chosen flyout (Pop Out Dashboard) size. */
export function setFlyoutSize(width: number, height: number): Promise<void> {
  return invoke<void>("set_flyout_size", { width, height });
}

/** The remembered flyout size ([w, h]) if the user has resized it, else null. */
export function flyoutStoredSize(): Promise<[number, number] | null> {
  return invoke<[number, number] | null>("flyout_stored_size");
}

export function quitApp(): Promise<void> {
  return invoke<void>("quit_app");
}
