//! Locale module for UI internationalization
//!
//! Provides localized strings for the application UI surfaces.
//! The locale is determined by the user's language setting in Settings.

use crate::settings::Language;
use crate::settings::Settings;
use fluent_templates::{Loader, static_loader};
use std::sync::LazyLock;
use unic_langid::LanguageIdentifier;

static_loader! {
    static LOCALES = {
        locales: "./src/locale",
        fallback_language: "en-US",
        customise: |bundle| bundle.set_use_isolating(false),
    };
}

/// Get the localized string for a given key in the specified language
pub fn get_text(lang: Language, key: LocaleKey) -> String {
    LOCALES
        .try_lookup(language_id(lang), key.name())
        .unwrap_or_else(|| key.name().to_string())
}

/// Replace `{}` placeholders in a template sequentially.
/// Safe for templates with multiple placeholders; each arg replaces the next occurrence.
pub fn format_template(template: &str, args: &[&str]) -> String {
    let mut result = template.to_string();
    for arg in args {
        if let Some(pos) = result.find("{}") {
            result.replace_range(pos..pos + 2, arg);
        } else {
            break;
        }
    }
    result
}

/// Get a localized string and replace `{}` placeholders sequentially.
pub fn format_locale(lang: Language, key: LocaleKey, args: &[&str]) -> String {
    format_template(&get_text(lang, key), args)
}

// Ceiling ships English and Simplified Chinese bundles. Languages without their
// own `.ftl` fall back to English via Fluent's `fallback_language`, so any key
// missing from a translation still resolves to readable English text.
fn language_id(lang: Language) -> &'static LanguageIdentifier {
    static EN_US: LazyLock<LanguageIdentifier> = LazyLock::new(|| "en-US".parse().unwrap());
    static ZH_CN: LazyLock<LanguageIdentifier> = LazyLock::new(|| "zh-CN".parse().unwrap());
    match lang {
        Language::Chinese => &ZH_CN,
        _ => &EN_US,
    }
}

/// Get the current UI language from settings
pub fn current_language() -> Language {
    Settings::load().ui_language
}

/// Get the localized tooltip for single-tray usage display
/// Format: "Provider: Session X% | Weekly Y%"
pub fn tray_tooltip(provider_name: &str, session_percent: f64, weekly_percent: f64) -> String {
    let lang = current_language();
    let session_label = get_text(lang, LocaleKey::TraySessionPercent);
    let weekly_label = get_text(lang, LocaleKey::TrayWeeklyPercent);
    format!(
        "{}: {} | {}",
        provider_name,
        session_label.replace("{}", &format!("{}", session_percent as i32)),
        weekly_label.replace("{}", &format!("{}", weekly_percent as i32))
    )
}

/// Get the localized tooltip for single-tray usage display with status overlay
/// Format: "Provider: Session X% | Weekly Y% (Status)"
pub fn tray_tooltip_with_status(
    provider_name: &str,
    session_percent: f64,
    weekly_percent: f64,
    status: Option<IconOverlayStatus>,
) -> String {
    let lang = current_language();
    let session_label = get_text(lang, LocaleKey::TraySessionPercent);
    let weekly_label = get_text(lang, LocaleKey::TrayWeeklyPercent);
    let status_suffix = match status {
        None => String::new(),
        Some(IconOverlayStatus::Error) => get_text(lang, LocaleKey::TrayStatusError),
        Some(IconOverlayStatus::Stale) => get_text(lang, LocaleKey::TrayStatusStale),
        Some(IconOverlayStatus::Incident) => get_text(lang, LocaleKey::TrayStatusIncident),
        Some(IconOverlayStatus::Partial) => get_text(lang, LocaleKey::TrayStatusPartial),
    };
    format!(
        "{}: {} | {}{}",
        provider_name,
        session_label.replace("{}", &format!("{}", session_percent as i32)),
        weekly_label.replace("{}", &format!("{}", weekly_percent as i32)),
        status_suffix
    )
}

/// Status overlay types for tray tooltips
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconOverlayStatus {
    Error,
    Stale,
    Incident,
    Partial,
}

/// Get the localized tooltip for credits mode
/// Format: "Provider: Weekly quota exhausted | Credits remaining X%"
pub fn tray_tooltip_credits(provider_name: &str, credits_percent: f64) -> String {
    let lang = current_language();
    let exhausted = get_text(lang, LocaleKey::TrayWeeklyExhausted);
    let credits = get_text(lang, LocaleKey::TrayCreditsRemaining);
    format!(
        "{}: {} | {}",
        provider_name,
        exhausted,
        credits.replace("{}", &format!("{:.0}", credits_percent))
    )
}

macro_rules! locale_keys {
    ($($key:ident,)*) => {
        /// Locale keys for app-owned UI strings.
        #[allow(dead_code)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum LocaleKey {
            $($key,)*
        }

        impl LocaleKey {
            /// Every LocaleKey variant paired with its serialized name.
            pub const ALL: &'static [(LocaleKey, &'static str)] = &[
                $((LocaleKey::$key, stringify!($key)),)*
            ];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$key => stringify!($key),)*
                }
            }
        }
    };
}

locale_keys! {

    // Tab names (Preferences)
    TabGeneral,
    TabProviders,
    TabNotifications,
    TabMenuBar,
    TabMenu,
    TabApiKeys,
    TabCookies,
    TabAdvanced,
    TabAbout,
    TabShortcuts,

    // General settings (Preferences)
    InterfaceLanguage,
    StartupSettings,
    StartAtLogin,
    StartMinimized,
    StartAtLoginHelper,
    StartMinimizedHelper,

    // Notification settings (Preferences)
    ShowNotificationsHelper,
    CapacityEventNotifications,
    CapacityEventNotificationsHelper,
    SoundEnabledHelper,
    HighUsageThresholdHelper,
    CriticalUsageThresholdHelper,

    // Notification settings (Preferences)
    ShowNotifications,
    SoundEnabled,
    SoundVolume,
    HighUsageThreshold,
    HighUsageAlert,
    CriticalUsageThreshold,
    CriticalUsageAlert,
    PredictivePaceWarnings,
    PredictivePaceWarningsHelper,
    PredictivePaceWarningTitle,
    PredictivePaceWarningBody,
    SectionSpendBudget,
    SpendBudgetAlerts,
    SpendBudgetAlertsHelper,
    SpendBudgetPeriod,
    SpendBudgetDaily,
    SpendBudgetMonthly,
    SpendBudgetWarning,
    SpendBudgetCap,
    SpendBudgetThresholdsHelper,
    SpendAnomalyAlerts,
    SpendAnomalyAlertsHelper,
    SpendAnomalyMultiplier,
    ProviderIncidentBadges,
    ProviderIncidentBadgesHelper,
    IncidentStatusPage,

    // Display settings (Preferences)
    UsageDisplay,
    ShowUsageAsUsed,
    ShowUsageAsUsedHelper,
    ResetTimeRelative,
    ResetTimeRelativeHelper,
    ShowResetWhenExhausted,
    ShowResetWhenExhaustedHelper,
    TrayIcon,

    // Provider settings (Preferences)
    ProviderEnabled,
    ProviderDisabled,
    ProviderInfo,
    ProviderUsage,
    AuthType,
    DataSource,
    ProviderNotDetected,
    ProviderLastFetchFailed,
    ProviderUsageNotFetchedYet,
    ProviderNotFetchedYetTitle,
    ProviderDisabledNoRecentData,
    ProviderSourceAutoShort,
    ProviderSourceWebShort,
    ProviderSourceCliShort,
    ProviderSourceOauthShort,
    ProviderSourceApiShort,
    ProviderSourceGithubApiShort,
    ProviderSourceLocalShort,
    ProviderSourceKiroEnvShort,
    WayfinderGatewayTitle,
    WayfinderGatewayLabel,
    WayfinderGatewayHelp,
    WayfinderGatewayStatus,
    WayfinderModels,
    WayfinderRequests,
    WayfinderTokens,
    WayfinderSaved,
    WayfinderOffline,
    WayfinderDryRun,
    WayfinderMissingKeys,
    TrackingItem,
    MainWindowLiveUsageData,
    StartTrackingUsage,
    ClickTrayIconForMetrics,

    // Browser cookie import (Preferences)
    BrowserCookieImport,
    ImportCookies,
    ImportSuccess,
    ImportFailed,
    SaveFailed,
    CookiesAutoImport,
    QuickActions,
    OpenProviderDashboard,
    OllamaNoDashboard,

    // API Keys tab (Preferences)
    ApiKeysTitle,
    ApiKeysDescription,
    AddKey,
    KeySet,
    KeyRequired,
    Remove,
    GetKey,

    // Cookies tab (Preferences)
    SavedCookies,
    AddManualCookie,
    CookieHeader,
    PasteHere,
    DeleteCookie,
    CookieSaved,
    CookieDeleted,

    // Advanced tab (Preferences)
    RefreshSettings,
    Animations,
    MenuBar,
    Fun,
    GlobalShortcut,
    Privacy,
    Updates,
    UpdateChannel,
    UpdateChannelStable,
    UpdateChannelBeta,
    Never,
    LastUpdated,
    NeverUpdated,
    MinutesAgo,
    HoursAgo,
    DaysAgo,
    BuiltWithRust,
    OriginalMacOSVersion,
    Links,
    BuildInfo,
    EnabledProviders,
    Appearance,
    ThemeSelection,
    LightMode,
    DarkMode,

    // About (Preferences)
    AboutTitle,
    Version,

    // Main popup - Header actions
    ActionRefreshAll,
    ActionSettings,
    ActionClose,

    // Main popup - Provider section
    ProviderAccount,
    ProviderSession,
    ProviderWeekly,
    ProviderMonthly,
    ProviderModel,
    ProviderPlan,
    ProviderNextReset,
    ProviderNoRecentUsage,
    ProviderNotSignedIn,
    SummaryTab,

    // Main popup - Loading/Empty/Error states (non-happy-path)
    StateLoadingProviders,
    StateNoProviderData,
    StateNoProviderSelected,
    StateSummaryRefreshPending,
    StateError,
    StateRetry,
    StateDownload,
    StateRestartAndUpdate,

    // Main popup - Credits
    CreditsTitle,

    // Main popup - Update banner (non-happy-path)
    UpdateRestartAndUpdate,
    UpdateRetry,
    UpdateDownload,
    UpdateDownloading,
    UpdateReady,
    UpdateFailed,

    // Main popup - Settings button
    ButtonOpenProviderSettings,

    // Main popup - Bottom menu (Actions)
    MenuSettings,
    MenuAbout,
    MenuQuit,

    // Main popup - Status strings
    StatusJustUpdated,
    StatusUnableToGetUsage,

    // Main popup - Provider detail actions
    ActionRefresh,
    ActionSwitchAccount,
    LoginPhaseRequesting,
    LoginPhaseWaitingBrowser,
    LoginPhaseEnterGithubCodePrefix,
    LoginPhaseOpenVerificationLink,
    LoginPhaseComplete,
    LoginPhaseFailed,
    ActionUsageDashboard,
    ActionStatusPage,
    ActionCopyError,
    ActionBuyCredits,

    // Main popup - Pace status
    PaceOnTrack,
    PaceBehind,

    // Main popup - Reset prefix
    MetricResetsIn,

    // Main popup - Section titles
    SectionUsageBreakdown,
    SectionCost,

    // Main popup - Usage/reset labels
    ResetInProgress,
    TomorrowAt,
    UsedPercent,
    RemainingPercent,
    RemainingAmount,
    Tokens1K,
    TodayCost,
    Last30DaysCost,
    StatusLabel,

    // Tray - Single icon mode
    TrayPopOutDashboard,
    TrayShowWindow,
    TrayShowTaskbarUsage,
    TrayRefreshAll,
    TrayProviders,
    TraySettings,
    TrayCheckForUpdates,
    TrayQuit,
    TrayLoading,
    TrayNoProviders,
    TraySessionPercent,
    TrayWeeklyPercent,
    TrayStatusError,
    TrayStatusStale,
    TrayStatusIncident,
    TrayStatusPartial,
    TrayWeeklyExhausted,
    TrayCreditsRemaining,
    TrayStatusRowLoading,
    TrayStatusRowError,
    TrayCreditsRow,

    // Tray - Per-provider mode
    TrayProviderPopOut,
    TrayProviderRefresh,
    TrayProviderSettings,
    TrayProviderQuit,

    // Provider settings - Live renderer specific
    State,
    Source,
    Updated,
    UpdatedJustNow,
    UpdatedMinutesAgo,
    UpdatedHoursAgo,
    UpdatedDaysAgo,
    UpdatedMinuteAgo,
    UpdatedHourAgo,
    UpdatedDayAgo,
    Status,
    AllSystemsOperational,
    Plan,
    Account,

    // Provider detail - Usage section
    ProviderSessionLabel,
    ProviderWeeklyLabel,
    ProviderCodeReviewLabel,
    ResetsInShort,
    ResetsInDays,
    ResetsInDaysHours,
    ResetsInHoursMinutes,
    ResetsInMinutes,

    // Provider detail - Tray Display
    TrayDisplayTitle,
    ShowInTray,

    // Provider detail - Credits
    CreditsLabel,
    CreditsLeft,

    // Provider detail - Cost
    CostTitle,
    TodayCostFull,
    Last30DaysCostFull,

    // Provider detail - Settings section
    ProviderSettingsTitle,
    ProviderAccountsTitle,
    ProviderOptionsTitle,
    MenuBarMetric,
    MenuBarMetricHelper,
    UsageSource,
    ProviderNoCodexAccountsDetected,
    ProviderCodexAutoImportHelp,
    ProviderCodexHistoryHelp,
    ProviderOpenAiCookies,
    ProviderHistoricalTracking,
    ProviderOpenAiWebExtras,
    ProviderOpenAiWebExtrasHelp,
    ProviderCodexCreditsUnavailable,
    ProviderCodexLastFetchFailedTitle,
    ProviderCodexNotRunningHelp,
    ProviderCookieSource,
    CookieSourceManual,
    ProviderRegion,
    ProviderClaudeCookies,
    ProviderClaudeCookiesHelp,
    ProviderClaudeAvoidKeychainPrompts,
    ProviderClaudeAvoidKeychainPromptsHelp,
    ProviderCodexSparkUsage,
    ProviderCodexSparkUsageHelp,
    ProviderCursorCookieSourceHelp,
    ProviderCursorCreditsHelp,
    AutoFallbackHelp,
    ProviderSourceOauthWeb,
    Automatic,
    Average,
    ExtraUsage,
    OAuth,
    Api,
    Web,

    // General tab sections
    PrivacyTitle,
    HidePersonalInfo,
    HidePersonalInfoHelper,
    SectionLocalIntegrations,
    PowerToysPipeLabel,
    PowerToysPipeHelper,
    AgentSessionsTitle,
    AgentSessionsEnableLabel,
    AgentSessionsEnableHelper,
    AgentSessionsSshHostsLabel,
    AgentSessionsSshHostsHelper,
    AgentSessionsLoading,
    AgentSessionsEmpty,
    UpdatesTitle,
    UpdateChannelChoice,
    UpdateChannelChoiceHelper,
    AutoDownloadUpdates,
    AutoDownloadUpdatesHelper,
    InstallUpdatesOnQuit,
    InstallUpdatesOnQuitHelper,

    // Keyboard shortcuts
    KeyboardShortcutsTitle,
    GlobalShortcutLabel,
    GlobalShortcutHelper,
    ShortcutFormatHint,
    Saved,
    InvalidFormat,
    ShortcutHintPlaceholder,

    // Display/Preferences helpers
    SelectProvider,

    // Refresh interval labels
    RefreshInterval30Sec,
    RefreshInterval1Min,
    RefreshInterval5Min,
    RefreshInterval10Min,

    // Cookies tab
    BrowserCookiesTitle,
    CookieImport,
    Provider,
    SelectPlaceholder,
    AutoRefreshInterval,

    // About tab - render_about_tab
    AboutDescription,
    AboutDescriptionLine2,
    ViewOnGitHub,
    SubmitIssue,
    MaintainedBy,
    CommitLabel,
    BuildDateLabel,

    // Shared form controls
    Save,
    Cancel,
    Label,
    Token,
    AddAccount,
    AccountAdded,
    AccountRemoved,
    AccountSwitched,
    AccountLabelHint,
    EnterApiKeyFor,
    PasteApiKeyHere,
    ApiKeySaved,
    ApiKeyRemoved,
    EnvironmentVariable,
    CookieSavedForProvider,
    CookieRemovedForProvider,

    // Usage helper functions
    ShowUsedPercent,
    ShowRemainingPercent,

    // Main popup - Update banner messages (non-happy-path)
    UpdateAvailableMessage,
    UpdateReadyMessage,
    UpdateFailedMessage,
    UpdateDownloadingMessage,

    // Tauri desktop shell — Settings section headings
    TabTokenAccounts,
    SectionRefresh,
    SectionNotifications,
    SectionUsageThresholds,
    SectionKeyboard,
    SectionUsageRendering,
    SectionTime,
    SectionLanguage,
    SectionCredentialsSecurity,
    SectionDebug,
    SectionApiKeys,
    SectionSavedCookies,
    SectionAddCookieManually,
    SectionTokenAccounts,
    SectionSavedAccounts,
    SectionAddAccount,

    // Tauri desktop shell — General tab fields
    RefreshIntervalLabel,
    RefreshIntervalHelper,
    RefreshAllProvidersOnMenuOpen,
    RefreshAllProvidersOnMenuOpenHelper,
    SoundVolumeHelper,
    HighUsageWarningHelper,
    CriticalUsageWarningHelper,
    GlobalShortcutFieldLabel,
    GlobalShortcutToggleHelper,
    TaskbarToggleShortcutFieldLabel,
    TaskbarToggleShortcutHelper,
    ShortcutRecordButton,
    ShortcutRecordingLabel,
    ShortcutRecordingHint,
    ShortcutClearButton,
    ShortcutEmptyPlaceholder,
    NotificationTestSound,
    NotificationTestSoundPlaying,
    NotificationTest,
    NotificationTestHelper,
    NotificationTestButton,
    NotificationTestSending,
    NotificationTestSent,
    NotificationTestFailed,

    // Tauri desktop shell — Display tab fields
    ShowProviderIcons,
    ShowProviderIconsHelper,
    PreferHighestUsage,
    PreferHighestUsageHelper,
    ShowPercentInTray,
    ShowPercentInTrayHelper,
    DisplayModeLabel,
    DisplayModeHelper,
    DisplayModeDetailed,
    DisplayModeCompact,
    DisplayModeMinimal,
    WindowScaleLabel,
    WindowScaleHelper,
    WindowScaleAriaLabel,
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
    ShowAsUsedLabel,
    ShowAsUsedHelper,
    ShowAllTokenAccountsLabel,
    ShowAllTokenAccountsHelper,
    EnableAnimationsLabel,
    EnableAnimationsHelper,
    // Tauri desktop shell — Advanced tab fields
    UpdateChannelStableOption,
    UpdateChannelBetaOption,
    CodexLocalLogsTitle,
    CodexLocalLogsCaption,
    CodexLogPathsLabel,
    CodexLogPathsHelper,
    AvoidKeychainPromptsLabel,
    AvoidKeychainPromptsHelper,
    DisableAllKeychainLabel,
    DisableAllKeychainHelper,
    // Tauri desktop shell — Theme (Phase 12)
    SectionTheme,
    ThemeLabel,
    ThemeHelper,
    ThemeAutoOption,
    ThemeLightOption,
    ThemeDarkOption,

    // Tauri desktop shell — settings status / common
    SettingsStatusSaving,
    ApiKeysTabHint,

    // Tauri desktop shell — tray / popout
    FetchingProviderData,
    NoProvidersConfigured,
    EnableProvidersHint,
    OpenSettingsButton,
    TooltipRefresh,
    TooltipSettings,
    TooltipPopOut,
    TooltipBackToTray,
    TrayCardErrorBadge,
    SummaryProvidersLabel,
    SummaryRefreshing,
    SummaryFailed,
    SummaryWithErrors,

    // Tauri desktop shell — provider detail
    DetailBackButton,
    DetailWindowPrimary,
    DetailWindowSecondary,
    DetailWindowModelSpecific,
    DetailWindowTertiary,
    DetailWindowMinutesSuffix,
    DetailWindowExhausted,
    DetailPaceTitle,
    DetailPaceOnTrack,
    DetailPaceSlightlyAhead,
    DetailPaceAhead,
    DetailPaceFarAhead,
    DetailPaceSlightlyBehind,
    DetailPaceBehind,
    DetailPaceFarBehind,
    DetailPaceRunsOutIn,
    DetailPaceWillLastToReset,
    DetailCostTitle,
    DetailCostUsed,
    DetailCostLimit,
    DetailCostRemaining,
    DetailCostResets,
    DetailChartCost,
    DetailChartCredits,
    DetailChartUsageBreakdown,
    DetailChartEmpty,
    MiniChartNoData,
    DetailUpdatedPrefix,
    PanelAllProviders,
    PanelAllProvidersShort,
    PanelShowAllProviders,
    PanelShowFewerProviders,
    PanelZoom,
    PanelMenu,
    PanelCopied,
    PanelToday,
    PanelThirtyDayCost,
    PanelThirtyDayTokens,
    PanelLatestTokens,
    PanelThirtyDayCostHistogram,
    PanelTopModelPrefix,
    PanelEstimatedFromLocalLogs,
    PanelEstimatedFromLocalLogsClaude,
    PanelEstimatedFromLocalLogsGrok,
    PanelExpected,
    PanelActual,
    PanelUsedSuffix,
    PanelLeftSuffix,
    PanelOnPaceBudget,
    PanelAmountOf,
    UsageExpectedByNow,
    PaceVerdictOnTrack,
    PaceVerdictAhead,
    PaceVerdictPlenty,
    PaceVerdictRunningOut,
    PaceVerdictRunsOutIn,
    PaceVerdictLastsToReset,
    PanelNow,
    PanelOneHour,
    PanelFiveHours,
    PanelTodayBudget,
    PanelReserveSuffix,
    PanelReserveLastsUntilReset,
    PanelReserveRunsOutInDaysHours,
    PanelReserveRunsOutInHours,
    FloatBarThirtyDayShort,
    FloatBarNoProviders,
    FloatBarRemainingSuffix,
    FloatBarShowCost,
    FloatBarShowCostDescription,

    // Tauri desktop shell — update banner
    BannerCheckingForUpdates,
    BannerUpdateAvailablePrefix,
    BannerDownloadButton,
    BannerViewRelease,
    BannerDismiss,
    BannerDownloadingPrefix,
    BannerReadyToInstallSuffix,
    BannerInstallRestart,
    BannerUpdateFailedPrefix,
    BannerRetry,

    // Tauri desktop shell — providers sidebar (Phase 6a)
    ProviderSidebarSearch,
    ProviderSidebarClearSearch,
    ProviderSidebarNoMatches,
    ProviderSidebarReorderHint,
    ProviderSidebarMoveUp,
    ProviderSidebarMoveDown,
    ProviderStatusOk,
    ProviderStatusStale,
    ProviderStatusError,
    ProviderStatusLoading,
    ProviderStatusDisabled,
    ProviderDetailPlaceholder,
    ProviderIssueNeedsSignIn,
    ProviderIssueFetchNeedsAttention,
    ProviderIssueCopy,
    ProviderIssueUnsupportedSourceModePrefix,
    CredentialStorageTitle,
    CredentialRevokeStored,
    CredentialApiKeys,
    CredentialManualCookies,
    CredentialTokenAccounts,
    CredentialProtectedPrefix,
    CredentialStatusNotCreated,
    CredentialStatusPlaintext,
    CredentialStatusUnavailable,
    CredentialStatusUnreadable,
    BrowserCookiesSectionTitle,
    BrowserCookieNoneSaved,
    BrowserCookieSavedBadge,
    BrowserCookieRemove,
    BrowserCookiePasteGuideTitle,
    BrowserCookiePasteGuideSignIn,
    BrowserCookiePasteGuideDevTools,
    BrowserCookiePasteGuideCopy,
    BrowserCookiePasteGuideSave,
    BrowserCookiePasteGuidePrivacy,
    BrowserCookieMigrationNotice,
    BrowserCookiePlaceholderDefault,
    BrowserCookiePlaceholderOllama,
    BrowserCookiePlaceholderCurl,
    BrowserCookieSave,
    SecretFieldReveal,
    SecretFieldHide,
    SecretFieldCookieLabel,
    SecretFieldTokenLabel,
    ConfirmCancel,
    ConfirmRemove,
    ConfirmRevoke,
    ConfirmRemoveCookieTitle,
    ConfirmRemoveCookieBody,
    ConfirmRemoveApiKeyTitle,
    ConfirmRemoveApiKeyBody,
    ConfirmRemoveTokenTitle,
    ConfirmRemoveTokenBody,
    ConfirmRevokeTitle,
    ConfirmRevokeBody,
    CredentialRemoved,
    CredentialRevoked,
    CredentialRemoveFailed,

    // Tauri desktop shell — Phase 6d credential detection UIs
    CredentialsSectionTitle,
    CredsStatusAuthenticated,
    CredsStatusNotSignedIn,
    CredsStatusDetected,
    CredsStatusNotDetected,
    CredsStatusAvailable,
    CredsStatusUnavailable,
    CredsOpenFolderAction,
    CredsRefreshDetectionAction,
    CredsSavePathAction,
    CredsBrowseAction,
    CredsGeminiCliLabel,
    CredsGeminiCliHelperPrefix,
    CredsGeminiCliSetupAction,
    CredsGeminiCliSetupHelp,
    CredsVertexAiLabel,
    CredsVertexAiHelperPrefix,
    CredsVertexAiSetupAction,
    CredsVertexAiSetupHelp,
    CredsJetBrainsLabel,
    CredsJetBrainsHelperDetectedPrefix,
    CredsJetBrainsHelperCustomPrefix,
    CredsJetBrainsHelperMissing,
    CredsJetBrainsCustomPathLabel,
    CredsJetBrainsCustomPathPlaceholder,
    CredsJetBrainsSelectLabel,
    CredsJetBrainsAutoDetectOption,
    CredsKiroLabel,
    CredsKiroHelperAvailablePrefix,
    CredsKiroHelperMissing,
    OpenCodeGoWorkspaceTitle,
    OpenCodeGoWorkspaceLabel,
    OpenCodeGoWorkspaceHelp,
    CredsOpenAiHistoryHelp,

    // Tauri desktop shell — Config-directory accounts (SOU-285)
    SectionAccounts,
    AccountsIntro,
    AccountsFollowingCli,
    AccountsFollowingCliHint,
    AccountsActive,
    AccountsSignedOut,
    AccountsTrackThis,
    AccountsRemove,
    AccountsEmpty,
    AccountsAddTitle,
    AccountsDirPlaceholder,
    AccountsLabelPlaceholder,
    AccountsCheckButton,
    AccountsAddButton,
    AccountsProbeSignedOut,
    AccountsProbeMissing,
    AccountsProbeAlreadyAdded,
    AccountsSetupHint,
    AccountsSetupTitle,
    AccountsSetupStep1,
    AccountsSetupStep2,
    AccountsSetupStep3,
    AccountsTint,

    // Tauri desktop shell — Token accounts (Phase 6e, review)
    TokenAccountActive,
    TokenAccountSetActive,
    TokenAccountRemove,
    TokenAccountAddButton,
    TokenAccountGithubLoginButton,
    TokenAccountEmpty,
    TokenAccountLabelPlaceholder,
    TokenAccountProviderLabel,
    TokenAccountProviderPlaceholder,
    TokenAccountAddedPrefix,
    TokenAccountUsedPrefix,
    TokenAccountTabHint,
    TokenAccountNoSupported,
    TokenAccountInlineSummary,

    // Phase 9 - Tray / pop-out pace badges + countdowns
    TrayPaceBadgeSlow,
    TrayPaceBadgeSteady,
    TrayPaceBadgeRacing,
    TrayPaceBadgeBurning,
    TrayResetsInLabel,
    TrayResetsDueNow,

    // Provider data-source / privacy explainer (SOU-179)
    DataSourceSectionTitle,
    DataSourcePrivacyNote,
    DataSourceLearnMore,
    DataSourceClaude,
    DataSourceCodex,
    DataSourceCursor,
    DataSourceCopilot,
    DataSourceGemini,
    DataSourceGrok,
    DataSourceGeneric,

    // First-run checklist (SOU-157)
    FirstRunTitle,
    FirstRunStepEnable,
    FirstRunStepAuth,
    FirstRunStepFloatbar,
    FirstRunOpenProviders,
    FirstRunOpenDisplay,
    FirstRunDismiss,

    // GitHub star prompt (SOU-311)
    StarPromptAriaLabel,
    StarPromptTitleRunning,
    StarPromptTitleUpdated,
    StarPromptBody,
    StarPromptStar,
    StarPromptLater,

    // Leftover English (SBS-156)
    StatusConnected,
    FreshnessStale,
    FreshnessError,
    NotCurrentlyEnforced,
    WindowUnavailable,
    // Tile-width spellings of the two above. A provider tile on a crowded
    // taskbar has ~51px for its headline, which "Unavailable" overruns, so the
    // strip needs a short form or it falls back to the error em dash (SBS-876).
    StripStateUnavailable,
    StripStateNotEnforced,
    TaskbarUsageTitle,
    ShowTaskbarUsage,
    ShowTaskbarUsageHelp,
    OpenOnHover,
    OpenOnHoverHelp,
    ShowOnAllMonitors,
    ShowOnAllMonitorsHelp,
    ShowResetTimeInline,
    ShowResetTimeInlineHelp,
    TaskbarWidgetShownOnOne,
    TaskbarWidgetShownOnMany,
    TaskbarWidgetNoFit,
    TaskbarWidgetWaitingLandmarks,
    TaskbarWidgetNoProviders,
    StripProvidersTitle,
    StripProvidersHelp,
    StripUseAutomaticOrder,
    StripKeepAtLeastOne,
    StripAlwaysOneProvider,
    StripEnableProvidersFirst,
    StripShowProvider,
    StripMoveUp,
    StripMoveDown,
    StripTaskbarShows,
    StripTaskbarAccount,
    StripAutoClosest,
    StripAccountHint,
    FloatingBarTitle,
    ShowFloatingBar,
    ShowFloatingBarHelp,
    FloatBarOrientation,
    FloatBarOrientationHelp,
    OrientationHorizontal,
    OrientationVertical,
    FloatBarDensity,
    FloatBarDensityHelp,
    DensityCompact,
    DensityStandard,
    DensityDetailed,
    FloatBarInformation,
    FloatBarInformationHelp,
    InformationExact,
    InformationCalm,
    FloatBarProvidersMode,
    FloatBarProvidersModeHelp,
    SelectionModePinned,
    SelectionModeActive,
    SelectionModeActivePlusCritical,
    WatchFocusedApp,
    WatchFocusedAppHelp,
    FloatBarContrast,
    FloatBarContrastHelp,
    ContrastLightText,
    ContrastDarkText,
    FloatBarOpacity,
    FloatBarOpacityHelp,
    FloatBarOpacityAria,
    FloatBarSize,
    FloatBarSizeHelp,
    FloatBarSizeAria,
    FloatBarClickThrough,
    FloatBarClickThroughHelp,
    AccountsGlanceEmptyTitle,
    AccountsGlanceEmptyBody,
    AccountsGlanceManage,
    AccountsGlanceAddOrManage,
    ChartsProviderTabs,
    ChartsCompare,
    ChartsNoChartsTitle,
    ChartsNoChartsBody,
    AboutLoading,
    AboutVersion,
    AboutCheckForUpdates,
    AboutChecking,
    AboutUpdateAvailable,
    AboutDownloading,
    AboutUpdateReady,
    AboutUpToDate,
    AboutUpdateCheckFailed,
    UpdateErrorNetwork,
    UpdateErrorHttp,
    UpdateErrorParse,
    UpdateErrorTimedOut,
    AboutLinkErrorPrefix,
    AboutCopyrightPrefix,
    AboutCopyrightMid,
    AboutCopyrightSuffix,
    ResetCreditsAvailableOne,
    ResetCreditsAvailableMany,
    FloatBarActions,
    FloatBarLock,
    FloatBarUnlock,
    FloatBarLockTitle,
    FloatBarUnlockTitle,
    FloatBarClickThroughMenu,
    FloatBarClickThroughTitle,
    FloatBarOpenSettings,
    FloatBarOpenSettingsTitle,
    FloatBarHide,
    FloatBarHideTitle,
    SettingsWindowTitle,

    // Activity heatmap card (SBS-972)
    ActivityHeatmapAriaLabel,
    ActivityHeatmapTitle,
    ActivityHeatmapSubtitle,
    ActivityHeatmapLocalTime,
    ActivityHeatmapReading,
    ActivityHeatmapReadFailed,
    ActivityHeatmapMetricGroup,
    ActivityHeatmapMetricApiValue,
    ActivityHeatmapMetricTokens,
    ActivityHeatmapProvidersGroup,
    ActivityHeatmapProvidersHidden,
    ActivityHeatmapEmpty,
    ActivityHeatmapByDay,
    ActivityHeatmapTotal,
    ActivityHeatmapDaysAria,
    ActivityHeatmapCellSummary,
    ActivityHeatmapCellSummaryUnpriced,
    ActivityHeatmapByHour,
    ActivityHeatmapBusiest,
    ActivityHeatmapNoPeak,
    ActivityHeatmapNoPricedDollars,
    ActivityHeatmapHoursAria,
    ActivityHeatmapHourCell,
    ActivityHeatmapHourCellUnpriced,
    ActivityHeatmapLess,
    ActivityHeatmapMore,
    ActivityHeatmapFooterNote,
    ActivityHeatmapTokensUnit,
}

#[cfg(test)]
mod tests;
