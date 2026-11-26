use super::*;

/// Raw on-disk shape of [`Settings`] used purely for deserialization.
///
/// It mirrors the canonical `Settings` fields but ALSO accepts the legacy
/// flat per-provider fields (`codex_cookie_source`, `alibaba_api_region`,
/// `claude_avoid_keychain_prompts`, …) so existing `settings.json` files keep
/// loading. The `From<RawSettings> for Settings` impl folds any present
/// legacy field into the unified [`provider_configs`](Settings::provider_configs)
/// map.
///
/// Saves go through `Settings`'s derived `Serialize`, which writes only the
/// new format (no legacy flat fields).
#[derive(Debug, Deserialize)]
#[serde(default)]
pub(super) struct RawSettings {
    enabled_providers: HashSet<String>,
    personal_gemini_migrated: bool,
    refresh_interval_secs: u64,
    refresh_all_providers_on_menu_open: bool,
    start_minimized: bool,
    start_at_login: bool,
    show_notifications: bool,
    #[serde(default = "default_true")]
    capacity_event_notifications_enabled: bool,
    sound_enabled: bool,
    sound_volume: u8,
    high_usage_threshold: f64,
    critical_usage_threshold: f64,
    #[serde(default)]
    spend_budget_alerts_enabled: bool,
    #[serde(default = "default_spend_budget_period")]
    spend_budget_period: String,
    #[serde(default = "default_spend_budget_warning_usd")]
    spend_budget_warning_usd: f64,
    #[serde(default = "default_spend_budget_limit_usd")]
    spend_budget_limit_usd: f64,
    #[serde(default)]
    spend_anomaly_alerts_enabled: bool,
    #[serde(default = "default_spend_anomaly_multiplier")]
    spend_anomaly_multiplier: f64,
    #[serde(default)]
    provider_incident_badges_enabled: bool,
    #[serde(default)]
    notification_policy_version: Option<u8>,
    provider_usage_thresholds: HashMap<String, UsageThresholdOverride>,
    #[serde(default = "default_true")]
    switcher_shows_icons: bool,
    menu_bar_shows_highest_usage: bool,
    menu_bar_shows_percent: bool,
    show_as_used: bool,
    enable_animations: bool,
    reset_time_relative: bool,
    show_reset_when_exhausted: bool,
    predictive_pace_warning_enabled: bool,
    menu_bar_display_mode: String,
    show_all_token_accounts_in_menu: bool,

    // ── New unified per-provider map ─────────────────────────────────
    //
    // Keyed by raw string rather than `ProviderId`. A `HashMap<ProviderId, _>`
    // makes serde reject the WHOLE document on meeting one provider id this
    // build does not know — a file written by a newer build, a renamed
    // `cli_name`, a hand-edited typo. `Settings::load` maps any parse failure
    // to `Settings::default()` and the next save persists those defaults, so a
    // single unknown key used to erase every unrelated preference (SBS-625).
    provider_configs: HashMap<String, ProviderConfig>,
    /// Provider ids a previous run could not resolve, parked here so a
    /// downgrade hands the entry back instead of deleting it.
    #[serde(default)]
    unrecognized_provider_configs: HashMap<String, ProviderConfig>,

    // ── Legacy flat per-provider fields (migrated on load) ───────────
    #[serde(default)]
    claude_usage_source: Option<String>,
    #[serde(default)]
    codex_usage_source: Option<String>,
    #[serde(default)]
    codex_cookie_source: Option<String>,
    #[serde(default)]
    codex_historical_tracking: Option<bool>,
    #[serde(default)]
    codex_openai_web_extras: Option<bool>,
    #[serde(default)]
    claude_cookie_source: Option<String>,
    #[serde(default)]
    cursor_cookie_source: Option<String>,
    #[serde(default)]
    opencode_cookie_source: Option<String>,
    #[serde(default)]
    opencode_workspace_id: Option<String>,
    #[serde(default)]
    factory_cookie_source: Option<String>,
    #[serde(default)]
    alibaba_cookie_source: Option<String>,
    #[serde(default)]
    alibaba_cookie_header: Option<String>,
    #[serde(default)]
    alibaba_api_region: Option<String>,
    #[serde(default)]
    kimi_cookie_source: Option<String>,
    #[serde(default)]
    kimi_manual_cookie_header: Option<String>,
    #[serde(default)]
    minimax_cookie_source: Option<String>,
    #[serde(default)]
    augment_cookie_source: Option<String>,
    #[serde(default)]
    augment_cookie_header: Option<String>,
    #[serde(default)]
    amp_cookie_source: Option<String>,
    #[serde(default)]
    amp_cookie_header: Option<String>,
    #[serde(default)]
    ollama_cookie_source: Option<String>,
    #[serde(default)]
    ollama_cookie_header: Option<String>,
    #[serde(default)]
    zai_api_region: Option<String>,
    #[serde(default)]
    jetbrains_ide_base_path: Option<String>,
    #[serde(default)]
    minimax_cookie_header: Option<String>,
    #[serde(default)]
    minimax_api_token: Option<String>,
    #[serde(default)]
    minimax_api_region: Option<String>,
    #[serde(default)]
    claude_avoid_keychain_prompts: Option<bool>,

    disable_keychain_access: bool,
    hide_personal_info: bool,
    #[serde(default, deserialize_with = "lenient")]
    update_channel: UpdateChannel,
    #[serde(default, deserialize_with = "lenient_map")]
    provider_metrics: HashMap<String, MetricPreference>,
    provider_order: Vec<String>,
    #[serde(default = "default_global_shortcut")]
    global_shortcut: String,
    #[serde(default = "default_taskbar_toggle_shortcut")]
    taskbar_toggle_shortcut: String,
    codex_custom_sessions_dirs: Vec<String>,
    agent_sessions_enabled: bool,
    agent_session_ssh_hosts: Vec<String>,
    auto_download_updates: bool,
    install_updates_on_quit: bool,
    #[serde(default, deserialize_with = "lenient")]
    ui_language: Language,
    #[serde(default, deserialize_with = "lenient")]
    theme: ThemePreference,
    #[serde(default = "default_window_scale_percent")]
    window_scale_percent: u16,
    #[serde(default = "default_tray_scale_percent")]
    tray_scale_percent: u16,
    #[serde(default)]
    powertoys_status_pipe_enabled: bool,

    #[serde(default)]
    float_bar_enabled: Option<bool>,
    #[serde(default)]
    taskbar_widget_enabled: Option<bool>,
    #[serde(default)]
    taskbar_widget_all_monitors: bool,
    #[serde(default = "default_float_bar_opacity")]
    float_bar_opacity: u8,
    #[serde(default = "default_float_bar_scale")]
    float_bar_scale: u8,
    #[serde(default = "default_float_bar_orientation")]
    float_bar_orientation: String,
    #[serde(default)]
    float_bar_style: Option<String>,
    #[serde(default = "default_true")]
    taskbar_widget_open_on_hover: bool,
    #[serde(default = "default_float_bar_density")]
    float_bar_density: String,
    #[serde(default = "default_float_bar_information_mode")]
    float_bar_information_mode: String,
    #[serde(default = "default_float_bar_selection_mode")]
    float_bar_selection_mode: String,
    #[serde(default = "default_true")]
    float_bar_foreground_detection: bool,
    #[serde(default)]
    float_bar_contrast: Option<String>,
    #[serde(default)]
    float_bar_click_through: bool,
    #[serde(default)]
    float_bar_provider_ids: Vec<String>,
    #[serde(default)]
    taskbar_account_by_provider: std::collections::HashMap<String, String>,
    #[serde(default)]
    float_bar_dark_text: bool,
    // Must stay `default_true`, not a bare `#[serde(default)]`. A bare attribute
    // uses the FIELD TYPE's default (`false`) rather than the container's, which
    // is what previously made a fresh install show the inline reset while every
    // upgraded install hid it.
    #[serde(default = "default_true")]
    float_bar_show_reset_inline: bool,
    #[serde(default)]
    float_bar_show_cost: bool,
}

impl Default for RawSettings {
    fn default() -> Self {
        let s = Settings::default();
        Self {
            enabled_providers: s.enabled_providers,
            personal_gemini_migrated: s.personal_gemini_migrated,
            refresh_interval_secs: s.refresh_interval_secs,
            refresh_all_providers_on_menu_open: s.refresh_all_providers_on_menu_open,
            start_minimized: s.start_minimized,
            start_at_login: s.start_at_login,
            show_notifications: s.show_notifications,
            capacity_event_notifications_enabled: s.capacity_event_notifications_enabled,
            sound_enabled: s.sound_enabled,
            sound_volume: s.sound_volume,
            high_usage_threshold: s.high_usage_threshold,
            critical_usage_threshold: s.critical_usage_threshold,
            spend_budget_alerts_enabled: s.spend_budget_alerts_enabled,
            spend_budget_period: s.spend_budget_period,
            spend_budget_warning_usd: s.spend_budget_warning_usd,
            spend_budget_limit_usd: s.spend_budget_limit_usd,
            spend_anomaly_alerts_enabled: s.spend_anomaly_alerts_enabled,
            spend_anomaly_multiplier: s.spend_anomaly_multiplier,
            provider_incident_badges_enabled: s.provider_incident_badges_enabled,
            notification_policy_version: Some(s.notification_policy_version),
            provider_usage_thresholds: HashMap::new(),
            switcher_shows_icons: s.switcher_shows_icons,
            menu_bar_shows_highest_usage: s.menu_bar_shows_highest_usage,
            menu_bar_shows_percent: s.menu_bar_shows_percent,
            show_as_used: s.show_as_used,
            enable_animations: s.enable_animations,
            reset_time_relative: s.reset_time_relative,
            show_reset_when_exhausted: s.show_reset_when_exhausted,
            predictive_pace_warning_enabled: s.predictive_pace_warning_enabled,
            menu_bar_display_mode: s.menu_bar_display_mode,
            show_all_token_accounts_in_menu: s.show_all_token_accounts_in_menu,
            provider_configs: s
                .provider_configs
                .into_iter()
                .map(|(id, config)| (id.cli_name().to_string(), config))
                .collect(),
            unrecognized_provider_configs: s.unrecognized_provider_configs,
            claude_usage_source: None,
            codex_usage_source: None,
            codex_cookie_source: None,
            codex_historical_tracking: None,
            codex_openai_web_extras: None,
            claude_cookie_source: None,
            cursor_cookie_source: None,
            opencode_cookie_source: None,
            opencode_workspace_id: None,
            factory_cookie_source: None,
            alibaba_cookie_source: None,
            alibaba_cookie_header: None,
            alibaba_api_region: None,
            kimi_cookie_source: None,
            kimi_manual_cookie_header: None,
            minimax_cookie_source: None,
            augment_cookie_source: None,
            augment_cookie_header: None,
            amp_cookie_source: None,
            amp_cookie_header: None,
            ollama_cookie_source: None,
            ollama_cookie_header: None,
            zai_api_region: None,
            jetbrains_ide_base_path: None,
            minimax_cookie_header: None,
            minimax_api_token: None,
            minimax_api_region: None,
            claude_avoid_keychain_prompts: None,
            disable_keychain_access: s.disable_keychain_access,
            hide_personal_info: s.hide_personal_info,
            update_channel: s.update_channel,
            provider_metrics: s.provider_metrics,
            provider_order: s.provider_order,
            global_shortcut: s.global_shortcut,
            taskbar_toggle_shortcut: s.taskbar_toggle_shortcut,
            codex_custom_sessions_dirs: s.codex_custom_sessions_dirs,
            agent_sessions_enabled: s.agent_sessions_enabled,
            agent_session_ssh_hosts: s.agent_session_ssh_hosts,
            auto_download_updates: s.auto_download_updates,
            install_updates_on_quit: s.install_updates_on_quit,
            ui_language: s.ui_language,
            theme: s.theme,
            window_scale_percent: s.window_scale_percent,
            tray_scale_percent: s.tray_scale_percent,
            powertoys_status_pipe_enabled: s.powertoys_status_pipe_enabled,
            float_bar_enabled: Some(s.float_bar_enabled),
            taskbar_widget_enabled: Some(s.taskbar_widget_enabled),
            taskbar_widget_all_monitors: s.taskbar_widget_all_monitors,
            float_bar_opacity: s.float_bar_opacity,
            float_bar_scale: s.float_bar_scale,
            float_bar_orientation: s.float_bar_orientation,
            float_bar_style: Some(s.float_bar_style),
            taskbar_widget_open_on_hover: s.taskbar_widget_open_on_hover,
            float_bar_density: s.float_bar_density,
            float_bar_information_mode: s.float_bar_information_mode,
            float_bar_selection_mode: s.float_bar_selection_mode,
            float_bar_foreground_detection: s.float_bar_foreground_detection,
            float_bar_contrast: s.float_bar_contrast,
            float_bar_click_through: s.float_bar_click_through,
            float_bar_provider_ids: s.float_bar_provider_ids,
            taskbar_account_by_provider: s.taskbar_account_by_provider,
            float_bar_dark_text: s.float_bar_dark_text,
            float_bar_show_reset_inline: s.float_bar_show_reset_inline,
            float_bar_show_cost: s.float_bar_show_cost,
        }
    }
}

/// Deserialize a value, falling back to its default when the stored value is
/// not one this build understands.
///
/// `#[serde(default)]` only covers a MISSING field. A field that is *present*
/// with an unrecognized value — a closed enum variant added by a newer build —
/// still fails the whole document, and `Settings::load` turns any parse
/// failure into defaults that the next save writes over the user's real file.
/// That is the SBS-625 data loss with a different trigger, so the closed enums
/// degrade to their own default instead of taking every sibling with them.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// Same contract per entry: one unreadable value drops that key, not the map.
/// Dropping the whole map would silently reset every provider's metric choice.
fn lenient_map<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::de::DeserializeOwned,
{
    let raw = HashMap::<String, serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(key, value)| V::deserialize(value).ok().map(|value| (key, value)))
        .collect())
}

/// Split the on-disk provider map into ids this build resolves and ids it does
/// not.
///
/// Unknown ids are preserved verbatim rather than dropped: a build that
/// predates a provider (or postdates a rename) would otherwise delete a config
/// it merely failed to recognize the moment anything triggers a save.
fn split_provider_configs(
    stored: HashMap<String, ProviderConfig>,
    stashed: HashMap<String, ProviderConfig>,
) -> (
    HashMap<ProviderId, ProviderConfig>,
    HashMap<String, ProviderConfig>,
) {
    let mut known: HashMap<ProviderId, ProviderConfig> = HashMap::new();
    let mut unknown: HashMap<String, ProviderConfig> = HashMap::new();
    // `provider_configs` is canonical, so it is chained last and wins over a
    // stale stash entry for the same id.
    for (key, config) in stashed.into_iter().chain(stored) {
        match ProviderId::from_cli_name(&key) {
            Some(id) => {
                known.insert(id, config);
            }
            None => {
                unknown.insert(key, config);
            }
        }
    }
    (known, unknown)
}

impl From<RawSettings> for Settings {
    fn from(raw: RawSettings) -> Self {
        let (mut provider_configs, unrecognized_provider_configs) =
            split_provider_configs(raw.provider_configs, raw.unrecognized_provider_configs);
        let legacy_float_bar_style = match raw.float_bar_style.as_deref() {
            Some("taskbar") => Some("taskbar"),
            Some("floating") => Some("floating"),
            _ => None,
        };
        let legacy_float_bar_enabled = raw.float_bar_enabled.unwrap_or(false);
        let (taskbar_widget_enabled, float_bar_enabled) = match raw.taskbar_widget_enabled {
            Some(taskbar_enabled) => (taskbar_enabled, legacy_float_bar_enabled),
            None if legacy_float_bar_style == Some("taskbar") => (legacy_float_bar_enabled, false),
            None if legacy_float_bar_style == Some("floating") => (false, legacy_float_bar_enabled),
            None => {
                let defaults = Settings::default();
                (defaults.taskbar_widget_enabled, defaults.float_bar_enabled)
            }
        };

        // Helper closures to lazily insert per-provider configs from legacy
        // flat fields. Existing `provider_configs` entries take precedence.
        fn set_cookie_source(
            map: &mut HashMap<ProviderId, ProviderConfig>,
            id: ProviderId,
            value: Option<String>,
        ) {
            if let Some(v) = value {
                let entry = map.entry(id).or_default();
                if entry.cookie_source.is_none() {
                    entry.cookie_source = Some(v);
                }
            }
        }
        fn set_usage_source(
            map: &mut HashMap<ProviderId, ProviderConfig>,
            id: ProviderId,
            value: Option<String>,
        ) {
            if let Some(v) = value {
                let entry = map.entry(id).or_default();
                if entry.usage_source.is_none() {
                    entry.usage_source = Some(v);
                }
            }
        }
        fn set_region(
            map: &mut HashMap<ProviderId, ProviderConfig>,
            id: ProviderId,
            value: Option<String>,
        ) {
            if let Some(v) = value {
                let entry = map.entry(id).or_default();
                if entry.api_region.is_none() {
                    entry.api_region = Some(v);
                }
            }
        }
        fn set_header(
            map: &mut HashMap<ProviderId, ProviderConfig>,
            id: ProviderId,
            value: Option<String>,
        ) {
            if let Some(v) = value {
                let entry = map.entry(id).or_default();
                if entry.manual_cookie_header.is_none() {
                    entry.manual_cookie_header = Some(v);
                }
            }
        }

        set_cookie_source(
            &mut provider_configs,
            ProviderId::Codex,
            raw.codex_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Claude,
            raw.claude_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Cursor,
            raw.cursor_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::OpenCode,
            raw.opencode_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Factory,
            raw.factory_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Alibaba,
            raw.alibaba_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Kimi,
            raw.kimi_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::MiniMax,
            raw.minimax_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Augment,
            raw.augment_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Amp,
            raw.amp_cookie_source,
        );
        set_cookie_source(
            &mut provider_configs,
            ProviderId::Ollama,
            raw.ollama_cookie_source,
        );

        set_usage_source(
            &mut provider_configs,
            ProviderId::Claude,
            raw.claude_usage_source,
        );
        set_usage_source(
            &mut provider_configs,
            ProviderId::Codex,
            raw.codex_usage_source,
        );

        set_region(
            &mut provider_configs,
            ProviderId::Alibaba,
            raw.alibaba_api_region,
        );
        set_region(&mut provider_configs, ProviderId::Zai, raw.zai_api_region);
        set_region(
            &mut provider_configs,
            ProviderId::MiniMax,
            raw.minimax_api_region,
        );

        set_header(
            &mut provider_configs,
            ProviderId::Alibaba,
            raw.alibaba_cookie_header,
        );
        set_header(
            &mut provider_configs,
            ProviderId::Kimi,
            raw.kimi_manual_cookie_header,
        );
        set_header(
            &mut provider_configs,
            ProviderId::Augment,
            raw.augment_cookie_header,
        );
        set_header(
            &mut provider_configs,
            ProviderId::Amp,
            raw.amp_cookie_header,
        );
        set_header(
            &mut provider_configs,
            ProviderId::Ollama,
            raw.ollama_cookie_header,
        );
        set_header(
            &mut provider_configs,
            ProviderId::MiniMax,
            raw.minimax_cookie_header,
        );

        if let Some(v) = raw.opencode_workspace_id {
            let entry = provider_configs.entry(ProviderId::OpenCode).or_default();
            if entry.workspace_id.is_none() {
                entry.workspace_id = Some(v);
            }
        }
        if let Some(v) = raw.minimax_api_token {
            let entry = provider_configs.entry(ProviderId::MiniMax).or_default();
            if entry.api_token.is_none() {
                entry.api_token = Some(v);
            }
        }
        if let Some(v) = raw.jetbrains_ide_base_path {
            let entry = provider_configs.entry(ProviderId::JetBrains).or_default();
            if entry.ide_base_path.is_none() {
                entry.ide_base_path = Some(v);
            }
        }
        if let Some(v) = raw.codex_openai_web_extras {
            let entry = provider_configs.entry(ProviderId::Codex).or_default();
            if entry.openai_web_extras.is_none() {
                entry.openai_web_extras = Some(v);
            }
        }
        if let Some(v) = raw.codex_historical_tracking
            && v
        {
            provider_configs
                .entry(ProviderId::Codex)
                .or_default()
                .historical_tracking = true;
        }
        if let Some(v) = raw.claude_avoid_keychain_prompts
            && v
        {
            provider_configs
                .entry(ProviderId::Claude)
                .or_default()
                .avoid_keychain_prompts = true;
        }

        let notification_policy_version = raw.notification_policy_version.unwrap_or_default();
        let high_usage_threshold = if notification_policy_version < NOTIFICATION_POLICY_VERSION
            && (raw.high_usage_threshold - 70.0).abs() < f64::EPSILON
        {
            85.0
        } else {
            raw.high_usage_threshold
        };

        let spend_budget_limit_usd = normalize_spend_budget_usd(raw.spend_budget_limit_usd);
        let spend_budget_warning_usd =
            normalize_spend_budget_usd(raw.spend_budget_warning_usd).min(spend_budget_limit_usd);

        Settings {
            enabled_providers: raw.enabled_providers,
            personal_gemini_migrated: raw.personal_gemini_migrated,
            refresh_interval_secs: raw.refresh_interval_secs,
            refresh_all_providers_on_menu_open: raw.refresh_all_providers_on_menu_open,
            start_minimized: raw.start_minimized,
            start_at_login: raw.start_at_login,
            show_notifications: raw.show_notifications,
            capacity_event_notifications_enabled: raw.capacity_event_notifications_enabled,
            sound_enabled: raw.sound_enabled,
            sound_volume: raw.sound_volume,
            high_usage_threshold,
            critical_usage_threshold: raw.critical_usage_threshold,
            spend_budget_alerts_enabled: raw.spend_budget_alerts_enabled,
            spend_budget_period: normalize_spend_budget_period(&raw.spend_budget_period),
            spend_budget_warning_usd,
            spend_budget_limit_usd,
            spend_anomaly_alerts_enabled: raw.spend_anomaly_alerts_enabled,
            spend_anomaly_multiplier: normalize_spend_anomaly_multiplier(
                raw.spend_anomaly_multiplier,
            ),
            provider_incident_badges_enabled: raw.provider_incident_badges_enabled,
            notification_policy_version: NOTIFICATION_POLICY_VERSION,
            provider_usage_thresholds: normalize_usage_threshold_overrides(
                raw.provider_usage_thresholds,
            ),
            switcher_shows_icons: raw.switcher_shows_icons,
            menu_bar_shows_highest_usage: raw.menu_bar_shows_highest_usage,
            menu_bar_shows_percent: raw.menu_bar_shows_percent,
            show_as_used: raw.show_as_used,
            enable_animations: raw.enable_animations,
            reset_time_relative: raw.reset_time_relative,
            show_reset_when_exhausted: raw.show_reset_when_exhausted,
            predictive_pace_warning_enabled: raw.predictive_pace_warning_enabled,
            menu_bar_display_mode: raw.menu_bar_display_mode,
            show_all_token_accounts_in_menu: raw.show_all_token_accounts_in_menu,
            provider_configs,
            unrecognized_provider_configs,
            disable_keychain_access: raw.disable_keychain_access,
            hide_personal_info: raw.hide_personal_info,
            update_channel: raw.update_channel,
            provider_metrics: raw.provider_metrics,
            provider_order: if raw.provider_order.is_empty() {
                Vec::new()
            } else {
                normalize_provider_order(&raw.provider_order)
            },
            global_shortcut: raw.global_shortcut,
            taskbar_toggle_shortcut: raw.taskbar_toggle_shortcut,
            codex_custom_sessions_dirs: raw.codex_custom_sessions_dirs,
            agent_sessions_enabled: raw.agent_sessions_enabled,
            agent_session_ssh_hosts: raw.agent_session_ssh_hosts,
            auto_download_updates: raw.auto_download_updates,
            install_updates_on_quit: raw.install_updates_on_quit,
            ui_language: raw.ui_language,
            theme: raw.theme,
            window_scale_percent: clamp_window_scale_percent(raw.window_scale_percent),
            tray_scale_percent: clamp_tray_scale_percent(raw.tray_scale_percent),
            powertoys_status_pipe_enabled: raw.powertoys_status_pipe_enabled,
            float_bar_enabled,
            taskbar_widget_enabled,
            taskbar_widget_all_monitors: raw.taskbar_widget_all_monitors,
            float_bar_opacity: clamp_float_bar_opacity(raw.float_bar_opacity),
            float_bar_scale: clamp_float_bar_scale(raw.float_bar_scale),
            float_bar_orientation: normalize_float_bar_orientation(&raw.float_bar_orientation),
            float_bar_style: "floating".to_string(),
            taskbar_widget_open_on_hover: raw.taskbar_widget_open_on_hover,
            float_bar_density: normalize_float_bar_density(&raw.float_bar_density),
            float_bar_information_mode: normalize_float_bar_information_mode(
                &raw.float_bar_information_mode,
            ),
            float_bar_selection_mode: normalize_float_bar_selection_mode(
                &raw.float_bar_selection_mode,
            ),
            float_bar_foreground_detection: raw.float_bar_foreground_detection,
            float_bar_contrast: raw
                .float_bar_contrast
                .map(|value| normalize_float_bar_contrast(&value)),
            float_bar_click_through: raw.float_bar_click_through,
            float_bar_provider_ids: raw.float_bar_provider_ids,
            taskbar_account_by_provider: sanitize_taskbar_account_map(
                raw.taskbar_account_by_provider,
            ),
            float_bar_dark_text: raw.float_bar_dark_text,
            float_bar_show_reset_inline: raw.float_bar_show_reset_inline,
            float_bar_show_cost: raw.float_bar_show_cost,
        }
    }
}

/// Drop empty keys/values so "Auto" stays represented by a missing map entry.
fn sanitize_taskbar_account_map(
    map: std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    map.into_iter()
        .filter_map(|(provider, account)| {
            let provider = provider.trim().to_ascii_lowercase();
            let account = account.trim().to_string();
            if provider.is_empty() || account.is_empty() {
                None
            } else {
                Some((provider, account))
            }
        })
        .collect()
}
