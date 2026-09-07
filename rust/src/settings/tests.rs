use super::*;

#[test]
fn test_settings_default() {
    let settings = Settings::default();
    assert!(settings.enabled_providers.contains("claude"));
    assert!(settings.enabled_providers.contains("codex"));
    assert_eq!(settings.refresh_interval_secs, 300);
    assert!(settings.show_notifications);
    assert!(settings.capacity_event_notifications_enabled);
    assert_eq!(settings.high_usage_threshold, 85.0);
    assert_eq!(settings.float_bar_selection_mode, "pinned");
    assert!(settings.float_bar_foreground_detection);
    assert_eq!(settings.critical_usage_threshold, 90.0);
    assert_eq!(
        settings.notification_policy_version,
        NOTIFICATION_POLICY_VERSION
    );
    assert!(!settings.show_reset_when_exhausted);
    assert!(!settings.predictive_pace_warning_enabled);
    assert!(!settings.float_bar_show_cost);
}

#[test]
fn new_warning_and_reset_settings_are_backward_compatible() {
    let loaded: Settings = serde_json::from_str(
        r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300
        }"#,
    )
    .expect("parse legacy settings");

    assert!(!loaded.show_reset_when_exhausted);
    assert!(!loaded.predictive_pace_warning_enabled);
    assert!(loaded.capacity_event_notifications_enabled);
}

#[test]
fn predictive_pace_warning_opt_in_survives_the_raw_round_trip() {
    // This was previously pinned to false on every load, which made the
    // setting unreachable. Opting in must now stick.
    let s = Settings {
        predictive_pace_warning_enabled: true,
        ..Settings::default()
    };

    let json = serde_json::to_string(&s).expect("serialize");
    let back: Settings = serde_json::from_str(&json).expect("deserialize");
    assert!(back.predictive_pace_warning_enabled);
}

#[test]
fn raw_settings_clamp_spend_budget_warning_to_cap() {
    let loaded: Settings = serde_json::from_str(
        r#"{
            "spend_budget_warning_usd": 99.0,
            "spend_budget_limit_usd": 10.0
        }"#,
    )
    .expect("parse settings");

    assert_eq!(loaded.spend_budget_warning_usd, 10.0);
    assert_eq!(loaded.spend_budget_limit_usd, 10.0);
}

#[test]
fn legacy_default_warning_threshold_migrates_to_85_percent() {
    let loaded: Settings = serde_json::from_str(
        r#"{
            "high_usage_threshold": 70.0,
            "critical_usage_threshold": 90.0
        }"#,
    )
    .expect("parse pre-policy settings");

    assert_eq!(loaded.high_usage_threshold, 85.0);
    assert_eq!(
        loaded.notification_policy_version,
        NOTIFICATION_POLICY_VERSION
    );
}

#[test]
fn notification_threshold_migration_preserves_custom_and_current_values() {
    let custom: Settings = serde_json::from_str(r#"{"high_usage_threshold": 75.0}"#)
        .expect("parse custom legacy threshold");
    assert_eq!(custom.high_usage_threshold, 75.0);

    let current: Settings = serde_json::from_str(
        r#"{
            "high_usage_threshold": 70.0,
            "notification_policy_version": 1
        }"#,
    )
    .expect("parse current policy threshold");
    assert_eq!(current.high_usage_threshold, 70.0);
}

#[test]
fn usage_thresholds_merge_provider_and_window_overrides() {
    let mut settings = Settings::default();
    settings.provider_usage_thresholds.insert(
        "codex".into(),
        UsageThresholdOverride {
            high: Some(75.0),
            critical: None,
        },
    );
    settings.provider_usage_thresholds.insert(
        "codex:weekly".into(),
        UsageThresholdOverride {
            high: None,
            critical: Some(95.0),
        },
    );

    assert_eq!(
        settings.usage_thresholds(ProviderId::Codex, "weekly"),
        UsageThresholds {
            high: 75.0,
            critical: 95.0,
        }
    );
    assert_eq!(
        settings.usage_thresholds(ProviderId::Claude, "session"),
        UsageThresholds {
            high: 85.0,
            critical: 90.0,
        }
    );
}

#[test]
fn empty_and_out_of_range_threshold_overrides_are_normalized_on_load() {
    let loaded: Settings = serde_json::from_str(
        r#"{
            "provider_usage_thresholds": {
                "codex": {"high": 120.0},
                "claude": {},
                "codex:weekly": {"critical": -10.0}
            }
        }"#,
    )
    .expect("parse settings");

    assert_eq!(loaded.provider_usage_thresholds.len(), 2);
    assert_eq!(loaded.provider_usage_thresholds["codex"].high, Some(100.0));
    assert_eq!(
        loaded.provider_usage_thresholds["codex:weekly"].critical,
        Some(0.0)
    );
}

#[test]
fn float_bar_defaults_are_safe() {
    let settings = Settings::default();
    assert!(!settings.float_bar_enabled);
    assert!(settings.taskbar_widget_enabled);
    assert!(!settings.taskbar_widget_all_monitors);
    assert_eq!(settings.float_bar_opacity, 80);
    assert_eq!(settings.float_bar_scale, 100);
    assert_eq!(settings.float_bar_orientation, "horizontal");
    assert_eq!(settings.float_bar_style, "floating");
    assert!(settings.taskbar_widget_open_on_hover);
    assert_eq!(settings.float_bar_density, "standard");
    assert_eq!(settings.float_bar_information_mode, "exact");
    assert_eq!(resolved_float_bar_contrast(&settings), "auto");
    assert!(!settings.float_bar_click_through);
    assert!(settings.float_bar_provider_ids.is_empty());
    assert!(settings.taskbar_account_by_provider.is_empty());
    assert!(!settings.float_bar_dark_text);
    assert!(settings.float_bar_show_reset_inline);
    assert!(!settings.float_bar_show_cost);
}

#[test]
fn main_window_scale_defaults_to_100_percent() {
    let settings = Settings::default();
    assert_eq!(settings.window_scale_percent, 100);
}

#[test]
fn main_window_scale_clamp_pins_to_supported_range() {
    assert_eq!(clamp_window_scale_percent(0), 100);
    assert_eq!(clamp_window_scale_percent(99), 100);
    assert_eq!(clamp_window_scale_percent(100), 100);
    assert_eq!(clamp_window_scale_percent(125), 125);
    assert_eq!(clamp_window_scale_percent(180), 180);
    assert_eq!(clamp_window_scale_percent(250), 250);
    assert_eq!(clamp_window_scale_percent(251), 250);
}

#[test]
fn raw_settings_clamps_main_window_scale_on_load() {
    let json = r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300,
            "window_scale_percent": 300
        }"#;
    let loaded: Settings = serde_json::from_str(json).expect("parse settings");
    assert_eq!(loaded.window_scale_percent, 250);
}

#[test]
fn tray_scale_defaults_to_100_percent() {
    let settings = Settings::default();
    assert_eq!(settings.tray_scale_percent, 100);
}

#[test]
fn tray_scale_clamp_pins_to_supported_range() {
    assert_eq!(clamp_tray_scale_percent(0), 100);
    assert_eq!(clamp_tray_scale_percent(99), 100);
    assert_eq!(clamp_tray_scale_percent(100), 100);
    assert_eq!(clamp_tray_scale_percent(125), 125);
    assert_eq!(clamp_tray_scale_percent(180), 180);
    assert_eq!(clamp_tray_scale_percent(200), 200);
    assert_eq!(clamp_tray_scale_percent(201), 200);
}

#[test]
fn raw_settings_clamps_tray_scale_on_load() {
    let json = r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300,
            "tray_scale_percent": 300
        }"#;
    let loaded: Settings = serde_json::from_str(json).expect("parse settings");
    assert_eq!(loaded.tray_scale_percent, 200);
}

#[test]
fn float_bar_opacity_clamp_pins_to_supported_range() {
    // Below 30 → 30 so the bar isn't accidentally invisible.
    assert_eq!(clamp_float_bar_opacity(0), 30);
    assert_eq!(clamp_float_bar_opacity(29), 30);
    // Within range → unchanged.
    assert_eq!(clamp_float_bar_opacity(45), 45);
    assert_eq!(clamp_float_bar_opacity(80), 80);
    // Above 100 → 100.
    assert_eq!(clamp_float_bar_opacity(150), 100);
    assert_eq!(clamp_float_bar_opacity(255), 100);
}

#[test]
fn float_bar_scale_clamp_pins_to_supported_range() {
    assert_eq!(clamp_float_bar_scale(0), 75);
    assert_eq!(clamp_float_bar_scale(74), 75);
    assert_eq!(clamp_float_bar_scale(100), 100);
    assert_eq!(clamp_float_bar_scale(150), 150);
    assert_eq!(clamp_float_bar_scale(250), 200);
}

#[test]
fn float_bar_orientation_normalization_rejects_unknown_values() {
    assert_eq!(normalize_float_bar_orientation("horizontal"), "horizontal");
    assert_eq!(normalize_float_bar_orientation("vertical"), "vertical");
    // Anything else collapses to horizontal so a corrupt settings file
    // can't poison the renderer with an unknown layout token.
    assert_eq!(normalize_float_bar_orientation(""), "horizontal");
    assert_eq!(normalize_float_bar_orientation("diagonal"), "horizontal");
    assert_eq!(normalize_float_bar_orientation("VERTICAL"), "horizontal");
}

#[test]
fn float_bar_style_normalization_rejects_unknown_values() {
    assert_eq!(normalize_float_bar_style("floating"), "floating");
    assert_eq!(normalize_float_bar_style("taskbar"), "taskbar");
    assert_eq!(normalize_float_bar_style(""), "floating");
    assert_eq!(normalize_float_bar_style("TASKBAR"), "floating");
    assert_eq!(normalize_float_bar_style("glass"), "floating");
}

#[test]
fn float_bar_density_and_contrast_normalization_reject_unknown_values() {
    assert_eq!(normalize_float_bar_density("compact"), "compact");
    assert_eq!(normalize_float_bar_density("standard"), "standard");
    assert_eq!(normalize_float_bar_density("detailed"), "detailed");
    assert_eq!(normalize_float_bar_density("dense"), "standard");

    assert_eq!(normalize_float_bar_information_mode("exact"), "exact");
    assert_eq!(normalize_float_bar_information_mode("calm"), "calm");
    // Unknown/older values migrate to exact, never silently into calm.
    assert_eq!(normalize_float_bar_information_mode(""), "exact");
    assert_eq!(normalize_float_bar_information_mode("Calm"), "exact");
    assert_eq!(normalize_float_bar_information_mode("minimal"), "exact");

    assert_eq!(normalize_float_bar_selection_mode("pinned"), "pinned");
    assert_eq!(normalize_float_bar_selection_mode("active"), "active");
    assert_eq!(
        normalize_float_bar_selection_mode("activePlusCritical"),
        "activePlusCritical"
    );
    assert_eq!(normalize_float_bar_selection_mode(""), "pinned");
    assert_eq!(normalize_float_bar_selection_mode("Active"), "pinned");

    assert_eq!(normalize_float_bar_contrast("auto"), "auto");
    assert_eq!(normalize_float_bar_contrast("light-text"), "light-text");
    assert_eq!(normalize_float_bar_contrast("dark-text"), "dark-text");
    assert_eq!(normalize_float_bar_contrast("inverted"), "auto");
}

#[test]
fn legacy_dark_text_setting_is_preserved_when_contrast_is_absent() {
    let mut settings = Settings {
        float_bar_contrast: None,
        float_bar_dark_text: true,
        ..Settings::default()
    };
    assert_eq!(resolved_float_bar_contrast(&settings), "dark-text");

    settings.float_bar_dark_text = false;
    assert_eq!(resolved_float_bar_contrast(&settings), "light-text");
}

#[test]
fn float_bar_settings_round_trip_through_raw() {
    // Serialize a Settings with custom float-bar values then deserialize
    // through the `from = "RawSettings"` path — values must survive intact
    // (after clamping/normalization).
    let s = Settings {
        float_bar_enabled: true,
        taskbar_widget_enabled: false,
        taskbar_widget_all_monitors: true,
        float_bar_opacity: 65,
        float_bar_scale: 140,
        float_bar_orientation: "vertical".to_string(),
        float_bar_style: "floating".to_string(),
        taskbar_widget_open_on_hover: false,
        float_bar_density: "compact".to_string(),
        float_bar_information_mode: "calm".to_string(),
        float_bar_selection_mode: "active".to_string(),
        float_bar_foreground_detection: false,
        float_bar_contrast: Some("dark-text".to_string()),
        float_bar_click_through: true,
        float_bar_provider_ids: vec!["claude".into(), "codex".into()],
        taskbar_account_by_provider: [("codex".into(), "work-id".into())].into_iter().collect(),
        float_bar_dark_text: true,
        float_bar_show_reset_inline: true,
        float_bar_show_cost: true,
        ..Settings::default()
    };

    let json = serde_json::to_string(&s).expect("serialize");
    let back: Settings = serde_json::from_str(&json).expect("deserialize");
    assert!(back.float_bar_enabled);
    assert!(!back.taskbar_widget_enabled);
    assert!(back.taskbar_widget_all_monitors);
    assert_eq!(back.float_bar_opacity, 65);
    assert_eq!(back.float_bar_scale, 140);
    assert_eq!(back.float_bar_orientation, "vertical");
    assert_eq!(back.float_bar_style, "floating");
    assert!(!back.taskbar_widget_open_on_hover);
    assert_eq!(back.float_bar_density, "compact");
    assert_eq!(back.float_bar_information_mode, "calm");
    assert_eq!(back.float_bar_selection_mode, "active");
    assert!(!back.float_bar_foreground_detection);
    assert_eq!(resolved_float_bar_contrast(&back), "dark-text");
    assert!(back.float_bar_click_through);
    assert_eq!(back.float_bar_provider_ids, vec!["claude", "codex"]);
    assert_eq!(back.taskbar_account_for("codex"), Some("work-id"));
    assert_eq!(back.taskbar_account_for("claude"), None);
    assert!(back.float_bar_dark_text);
    assert!(back.float_bar_show_reset_inline);
    assert!(back.float_bar_show_cost);
}

#[test]
fn float_bar_raw_clamps_out_of_range_opacity_on_load() {
    // Simulate an externally-edited settings.json with a wild opacity.
    let json = r#"{
            "enabled_providers": [],
            "refresh_interval_secs": 300,
            "start_minimized": false,
            "start_at_login": false,
            "show_notifications": true,
            "sound_enabled": true,
            "sound_volume": 100,
            "high_usage_threshold": 70.0,
            "critical_usage_threshold": 90.0,
            "show_as_used": true,
            "enable_animations": true,
            "reset_time_relative": true,
            "menu_bar_display_mode": "detailed",
            "disable_keychain_access": false,
            "hide_personal_info": false,
            "float_bar_opacity": 250,
            "float_bar_scale": 250,
            "float_bar_orientation": "diagonal",
            "float_bar_style": "glass"
        }"#;
    let loaded: Settings = serde_json::from_str(json).expect("parse");
    assert_eq!(loaded.float_bar_opacity, 100);
    assert_eq!(loaded.float_bar_scale, 200);
    assert_eq!(loaded.float_bar_orientation, "horizontal");
    assert_eq!(loaded.float_bar_style, "floating");
    assert!(loaded.taskbar_widget_enabled);
    assert!(!loaded.float_bar_enabled);
}

#[test]
fn legacy_floating_style_migrates_to_the_independent_floating_bar() {
    let loaded: Settings = serde_json::from_str(
        r#"{
            "float_bar_enabled": true,
            "float_bar_style": "floating"
        }"#,
    )
    .expect("parse legacy floating settings");

    assert!(loaded.float_bar_enabled);
    assert!(!loaded.taskbar_widget_enabled);
    assert_eq!(loaded.float_bar_style, "floating");
}

#[test]
fn test_settings_provider_enabled() {
    let settings = Settings::default();
    assert!(settings.is_provider_enabled(ProviderId::Claude));
    assert!(settings.is_provider_enabled(ProviderId::Codex));
    assert!(!settings.is_provider_enabled(ProviderId::Gemini));
    assert!(!settings.is_provider_enabled(ProviderId::Wayfinder));
    assert_eq!(
        settings.gateway_url(ProviderId::Wayfinder),
        "http://127.0.0.1:8088"
    );
}

#[test]
fn wayfinder_gateway_round_trips_without_changing_settings_paths() {
    let mut settings = Settings::default();
    settings.set_gateway_url(
        ProviderId::Wayfinder,
        "https://gateway.example.test/wayfinder/",
    );

    let json = serde_json::to_string(&settings).expect("serialize settings");
    let loaded: Settings = serde_json::from_str(&json).expect("deserialize settings");
    assert_eq!(
        loaded.gateway_url(ProviderId::Wayfinder),
        "https://gateway.example.test/wayfinder/"
    );
}

#[test]
fn test_settings_toggle_provider() {
    let mut settings = Settings::default();

    // Claude starts enabled
    assert!(settings.is_provider_enabled(ProviderId::Claude));

    // Toggle off
    let enabled = settings.toggle_provider(ProviderId::Claude);
    assert!(!enabled);
    assert!(!settings.is_provider_enabled(ProviderId::Claude));

    // Toggle back on
    let enabled = settings.toggle_provider(ProviderId::Claude);
    assert!(enabled);
    assert!(settings.is_provider_enabled(ProviderId::Claude));
}

#[test]
fn test_settings_get_enabled_provider_ids() {
    let settings = Settings::default();
    let enabled = settings.get_enabled_provider_ids();
    assert!(enabled.contains(&ProviderId::Claude));
    assert!(enabled.contains(&ProviderId::Codex));
}

#[test]
fn provider_order_dedupes_unknowns_and_appends_canonical_ids() {
    let order = normalize_provider_order(&[
        "gemini".to_string(),
        "not-a-provider".to_string(),
        "claude".to_string(),
        "gemini".to_string(),
    ]);

    assert_eq!(order[0], "gemini");
    assert_eq!(order[1], "claude");
    assert!(!order.iter().any(|id| id == "not-a-provider"));
    assert_eq!(order.len(), ProviderId::all().len());
}

#[test]
fn enabled_provider_ids_follow_custom_provider_order() {
    let settings = Settings {
        enabled_providers: ["claude", "codex", "gemini"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        provider_order: normalize_provider_order(&[
            "gemini".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ]),
        ..Settings::default()
    };

    assert_eq!(
        settings.get_enabled_provider_ids(),
        vec![ProviderId::Gemini, ProviderId::Claude, ProviderId::Codex]
    );
}

#[test]
fn test_settings_get_all_providers_status() {
    let settings = Settings::default();
    let status = settings.get_all_providers_status();
    assert_eq!(status.len(), ProviderId::all().len());

    let claude_status = status.iter().find(|s| s.id == "claude").unwrap();
    assert_eq!(claude_status.name, "Claude");
    assert!(claude_status.enabled);

    let gemini_status = status.iter().find(|s| s.id == "gemini").unwrap();
    assert!(!gemini_status.enabled);
}

#[test]
fn test_api_key_provider_catalog_includes_token_providers() {
    let providers = get_api_key_providers();
    for id in [
        ProviderId::Kilo,
        ProviderId::Bedrock,
        ProviderId::Codebuff,
        ProviderId::DeepSeek,
        ProviderId::ElevenLabs,
        ProviderId::Deepgram,
        ProviderId::Grok,
        ProviderId::Groq,
        ProviderId::LLMProxy,
    ] {
        assert!(
            providers.iter().any(|provider| provider.id == id),
            "{id} should be configurable from the API Keys UI"
        );
    }
}

#[test]
fn test_t3_chat_is_cookie_configured_not_api_key_configured() {
    let providers = get_api_key_providers();
    assert!(
        !providers
            .iter()
            .any(|provider| provider.id == ProviderId::T3Chat),
        "T3 Chat fetches usage from browser cookies or pasted cURL, not API keys"
    );
}

#[test]
fn test_refresh_interval_options() {
    let options = get_refresh_interval_options();
    assert!(!options.is_empty());
    assert!(options.iter().any(|o| o.value == 60));
    assert!(options.iter().any(|o| o.value == 300));
}

#[test]
fn test_manual_cookies_default() {
    let cookies = ManualCookies::default();
    assert!(cookies.cookies.is_empty());
}

#[test]
fn test_manual_cookies_set_get_remove() {
    let mut cookies = ManualCookies::default();

    // Set a cookie
    cookies.set("claude", "session=abc123");
    assert_eq!(cookies.get("claude"), Some("session=abc123"));

    // Remove it
    cookies.remove("claude");
    assert_eq!(cookies.get("claude"), None);
}

#[test]
fn legacy_credentials_replace_only_empty_secure_store_entries() {
    assert_eq!(
        legacy_credential_to_migrate(Some(" legacy-secret "), None),
        Some("legacy-secret")
    );
    assert_eq!(
        legacy_credential_to_migrate(Some("legacy-secret"), Some("  \t")),
        Some("legacy-secret")
    );
    assert_eq!(
        legacy_credential_to_migrate(Some("legacy-secret"), Some("current-secret")),
        None
    );
    assert_eq!(legacy_credential_to_migrate(Some("  "), None), None);
}

#[test]
fn api_key_display_mask_is_utf8_safe() {
    let mut keys = ApiKeys::default();
    keys.set("openrouter", "🔑🔒漢字abcdefgh🔐", Some("unicode"));

    let display = keys.get_all_for_display();

    assert_eq!(display.len(), 1);
    assert_eq!(display[0].masked_key, "🔑🔒漢字...fgh🔐");
}

#[test]
fn test_start_at_login_command_uses_only_the_executable_path() {
    let path = std::path::PathBuf::from(r"C:\Program Files\Ceiling\ceiling.exe");
    let command = Settings::start_at_login_command(&path);
    assert_eq!(command, "\"C:\\Program Files\\Ceiling\\ceiling.exe\"");
    assert!(!command.contains("menubar"));
}

#[test]
fn test_start_at_login_prefers_desktop_sibling_when_called_from_cli() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cli_path = temp.path().join("codexbar-cli.exe");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&cli_path, b"cli").expect("write cli");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");

    let command = Settings::start_at_login_command(&cli_path);

    assert_eq!(command, format!("\"{}\"", desktop_path.display()));
}

#[test]
fn test_start_at_login_keeps_current_exe_when_desktop_sibling_missing() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cli_path = temp.path().join("codexbar-cli.exe");
    std::fs::write(&cli_path, b"cli").expect("write cli");

    let command = Settings::start_at_login_command(&cli_path);

    assert_eq!(command, format!("\"{}\"", cli_path.display()));
}

#[test]
fn test_start_at_login_repairs_stale_cli_command_after_update() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cli_path = temp.path().join("codexbar-cli.exe");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&cli_path, b"cli").expect("write cli");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let stale_command = format!("\"{}\"", cli_path.display());

    assert!(Settings::start_at_login_command_needs_repair(
        &stale_command,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_keeps_current_desktop_command_after_update() {
    let temp = tempfile::tempdir().expect("temp dir");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let current_command = format!("\"{}\"", desktop_path.display());

    assert!(!Settings::start_at_login_command_needs_repair(
        &current_command,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_repairs_legacy_desktop_command_after_update() {
    let temp = tempfile::tempdir().expect("temp dir");
    let desktop_path = temp.path().join("ceiling.exe");
    let legacy_desktop_path = temp.path().join("codexbar-desktop.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    std::fs::write(&legacy_desktop_path, b"legacy desktop").expect("write legacy desktop");
    let stale_command = format!("\"{}\"", legacy_desktop_path.display());

    assert!(Settings::start_at_login_command_needs_repair(
        &stale_command,
        &legacy_desktop_path
    ));
}

#[test]
fn test_start_at_login_does_not_steal_another_tree() {
    let installed = tempfile::tempdir().expect("installed dir");
    let portable = tempfile::tempdir().expect("portable dir");
    let installed_desktop = installed.path().join("ceiling.exe");
    let portable_cli = portable.path().join("codexbar-cli.exe");
    let portable_desktop = portable.path().join("ceiling.exe");
    std::fs::write(&installed_desktop, b"installed").expect("write installed");
    std::fs::write(&portable_cli, b"cli").expect("write portable cli");
    std::fs::write(&portable_desktop, b"portable").expect("write portable desktop");
    let installed_command = format!("\"{}\"", installed_desktop.display());

    assert!(!Settings::start_at_login_command_needs_repair(
        &installed_command,
        &portable_cli
    ));
    assert!(!Settings::start_at_login_command_needs_repair(
        &installed_command,
        &portable_desktop
    ));
}

#[test]
fn test_start_at_login_preserves_custom_args() {
    let temp = tempfile::tempdir().expect("temp dir");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let command = format!("\"{}\" --minimized", desktop_path.display());

    assert!(!Settings::start_at_login_command_needs_repair(
        &command,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_preserves_custom_args_on_stale_sibling() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cli_path = temp.path().join("codexbar-cli.exe");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&cli_path, b"cli").expect("write cli");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let command = format!("\"{}\" --minimized", cli_path.display());

    assert!(!Settings::start_at_login_command_needs_repair(
        &command,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_preserves_unrelated_run_value() {
    let temp = tempfile::tempdir().expect("temp dir");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");

    assert!(!Settings::start_at_login_command_needs_repair(
        r#""D:\tools\other.exe""#,
        &desktop_path
    ));
    assert!(!Settings::start_at_login_command_needs_repair(
        "",
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_repairs_unquoted_owned_command() {
    let temp = tempfile::tempdir().expect("temp dir");
    let desktop_path = temp.path().join("ceiling.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let unquoted = desktop_path.display().to_string();

    assert!(Settings::start_at_login_command_needs_repair(
        &unquoted,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_repairs_unquoted_owned_command_with_spaces() {
    let temp = tempfile::tempdir().expect("temp dir");
    let install_dir = temp.path().join("Program Files").join("Ceiling");
    std::fs::create_dir_all(&install_dir).expect("create install dir");
    let desktop_path = install_dir.join("ceiling.exe");
    std::fs::write(&desktop_path, b"desktop").expect("write desktop");
    let unquoted = desktop_path.display().to_string();

    assert!(Settings::start_at_login_command_needs_repair(
        &unquoted,
        &desktop_path
    ));
}

#[test]
fn test_start_at_login_parses_an_exe_named_parent_directory() {
    let path = std::path::PathBuf::from(r"C:\Users\person.exe\Ceiling\ceiling.exe");
    let command = path.display().to_string();

    assert!(Settings::start_at_login_command_needs_repair(
        &command, &path
    ));
}

#[test]
fn test_start_at_login_ownership_includes_custom_args_but_excludes_foreign_entries() {
    let installed = std::path::PathBuf::from(r"C:\Program Files\Ceiling\ceiling.exe");

    assert!(Settings::start_at_login_command_is_owned(
        r#""C:\Program Files\Ceiling\ceiling.exe" --minimized"#,
        &installed
    ));
    assert!(!Settings::start_at_login_command_is_owned(
        r#""D:\Portable\Ceiling\ceiling.exe""#,
        &installed
    ));
    assert!(!Settings::start_at_login_command_is_owned(
        r#""C:\Tools\other.exe""#,
        &installed
    ));
}

#[test]
fn test_language_defaults_to_english() {
    let settings = Settings::default();
    assert_eq!(settings.ui_language, Language::English);
}

#[test]
fn test_language_all_variants_available() {
    let languages = Language::all();
    assert_eq!(languages.len(), 6);
    assert!(languages.contains(&Language::English));
    assert!(languages.contains(&Language::Chinese));
    assert!(languages.contains(&Language::ChineseTraditional));
    assert!(languages.contains(&Language::Japanese));
    assert!(languages.contains(&Language::Korean));
    assert!(languages.contains(&Language::Spanish));
}

#[test]
fn test_language_display_names() {
    assert_eq!(Language::English.display_name(), "English");
    assert_eq!(Language::Chinese.display_name(), "中文");
    assert_eq!(
        Language::ChineseTraditional.display_name(),
        "繁體中文（臺灣）"
    );
    assert_eq!(Language::Japanese.display_name(), "日本語");
}

#[test]
fn test_settings_load_missing_language_field_defaults_to_english() {
    // Simulate loading legacy settings JSON without ui_language field
    let legacy_json = r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300,
            "start_minimized": false,
            "ui_language": "english"
        }"#;

    let settings: Result<Settings, _> = serde_json::from_str(legacy_json);
    assert!(settings.is_ok());
    let settings = settings.unwrap();
    assert_eq!(settings.ui_language, Language::English);
}

#[test]
fn test_settings_roundtrip_with_language() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create settings with Chinese language
    let settings = Settings {
        ui_language: Language::Chinese,
        ..Settings::default()
    };

    // Save to a temp file
    let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
    let json = serde_json::to_string_pretty(&settings).expect("Failed to serialize settings");
    temp_file
        .write_all(json.as_bytes())
        .expect("Failed to write settings");
    let path = temp_file.path().to_path_buf();

    // Read back and verify
    let content = std::fs::read_to_string(&path).expect("Failed to read settings");
    let loaded: Settings = serde_json::from_str(&content).expect("Failed to deserialize settings");

    assert_eq!(loaded.ui_language, Language::Chinese);
}

#[test]
fn test_settings_with_utf8_bom_parses() {
    // Covers two things: a leading BOM, which some editors write, and that a
    // settings file still carrying the removed `tray_icon_mode` field loads
    // rather than failing for an existing user.
    let json = "\u{feff}{\n            \"enabled_providers\": [\"claude\", \"codex\"],\n            \"refresh_interval_secs\": 300,\n            \"tray_icon_mode\": \"perprovider\"\n        }";

    let settings: Settings = serde_json::from_str(json.trim_start_matches('\u{feff}')).unwrap();

    assert_eq!(settings.refresh_interval_secs, 300);
}

#[test]
fn test_language_serde_serialization() {
    // Test that Language serializes to lowercase string
    let english = Language::English;
    let chinese = Language::Chinese;
    let chinese_traditional = Language::ChineseTraditional;

    let english_json = serde_json::to_string(&english).unwrap();
    let chinese_json = serde_json::to_string(&chinese).unwrap();
    let chinese_traditional_json = serde_json::to_string(&chinese_traditional).unwrap();

    assert_eq!(english_json, "\"english\"");
    assert_eq!(chinese_json, "\"chinese\"");
    assert_eq!(chinese_traditional_json, "\"chinesetraditional\"");
}

#[test]
fn test_language_serde_deserialization() {
    // Test that lowercase strings deserialize correctly
    let english: Language = serde_json::from_str("\"english\"").unwrap();
    let chinese: Language = serde_json::from_str("\"chinese\"").unwrap();
    let chinese_traditional: Language = serde_json::from_str("\"chinesetraditional\"").unwrap();

    assert_eq!(english, Language::English);
    assert_eq!(chinese, Language::Chinese);
    assert_eq!(chinese_traditional, Language::ChineseTraditional);
}

#[test]
fn test_language_resolves_traditional_chinese_aliases() {
    assert_eq!(
        Language::resolve("chinesetraditional"),
        Some(Language::ChineseTraditional)
    );
    assert_eq!(
        Language::resolve("zh-tw"),
        Some(Language::ChineseTraditional)
    );
    assert_eq!(
        Language::resolve("zh-hant-tw"),
        Some(Language::ChineseTraditional)
    );
    assert_eq!(
        Language::resolve("繁體中文"),
        Some(Language::ChineseTraditional)
    );
}

#[test]
fn test_theme_defaults_to_auto() {
    let settings = Settings::default();
    assert_eq!(settings.theme, ThemePreference::Auto);
}

#[test]
fn test_theme_all_variants_available() {
    let themes = ThemePreference::all();
    assert_eq!(themes.len(), 3);
    assert!(themes.contains(&ThemePreference::Auto));
    assert!(themes.contains(&ThemePreference::Light));
    assert!(themes.contains(&ThemePreference::Dark));
}

#[test]
fn test_theme_serde_roundtrip() {
    for variant in [
        ThemePreference::Auto,
        ThemePreference::Light,
        ThemePreference::Dark,
    ] {
        let encoded = serde_json::to_string(&variant).unwrap();
        let decoded: ThemePreference = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, variant);
    }
    assert_eq!(
        serde_json::to_string(&ThemePreference::Light).unwrap(),
        "\"light\""
    );
    assert_eq!(
        serde_json::to_string(&ThemePreference::Dark).unwrap(),
        "\"dark\""
    );
    assert_eq!(
        serde_json::to_string(&ThemePreference::Auto).unwrap(),
        "\"auto\""
    );
}

#[test]
fn test_settings_missing_theme_defaults_to_auto() {
    // Legacy settings JSON without the theme field should still parse.
    let legacy_json = r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300,
            "ui_language": "english"
        }"#;

    let settings: Settings = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(settings.theme, ThemePreference::Auto);
}

#[test]
fn test_settings_roundtrip_with_theme() {
    let settings = Settings {
        theme: ThemePreference::Dark,
        ..Settings::default()
    };
    let json = serde_json::to_string(&settings).unwrap();
    let loaded: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.theme, ThemePreference::Dark);
}

// ── Phase 3: provider_configs migration tests ───────────────────────

/// Loading a legacy `settings.json` (with flat per-provider fields)
/// must populate `provider_configs` and surface every value through the
/// per-provider accessors.
#[test]
fn test_legacy_per_provider_fields_migrate_into_provider_configs() {
    // NOTE: placeholder values only — no real cookies/tokens.
    let legacy_json = r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300,
            "codex_cookie_source": "manual",
            "claude_cookie_source": "browser",
            "cursor_cookie_source": "manual",
            "alibaba_cookie_source": "manual",
            "alibaba_cookie_header": "ali=PLACEHOLDER",
            "alibaba_api_region": "cn",
            "zai_api_region": "cn",
            "minimax_api_region": "cn",
            "minimax_api_token": "TOK_PLACEHOLDER",
            "claude_usage_source": "ccusage",
            "codex_usage_source": "manual",
            "codex_openai_web_extras": false,
            "codex_historical_tracking": true,
            "claude_avoid_keychain_prompts": true,
            "opencode_workspace_id": "ws_placeholder",
            "jetbrains_ide_base_path": "C:/JB"
        }"#;

    let settings: Settings = serde_json::from_str(legacy_json).unwrap();

    // Cookie sources
    assert_eq!(settings.cookie_source(ProviderId::Codex), "manual");
    assert_eq!(settings.cookie_source(ProviderId::Claude), "browser");
    assert_eq!(settings.cookie_source(ProviderId::Cursor), "manual");
    assert_eq!(settings.cookie_source(ProviderId::Alibaba), "manual");
    // Untouched providers fall through to the default "manual" to avoid
    // background browser-cookie reads unless the user opts into Automatic.
    // Cursor is the exception: it defaults to Automatic so the IDE disk session works.
    assert_eq!(settings.cookie_source(ProviderId::Amp), "manual");
    assert_eq!(
        Settings::default().cookie_source(ProviderId::Cursor),
        "auto"
    );

    // Manual cookie headers + api regions
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Alibaba),
        "ali=PLACEHOLDER"
    );
    assert_eq!(settings.api_region(ProviderId::Alibaba), "cn");
    assert_eq!(settings.api_region(ProviderId::Zai), "cn");
    assert_eq!(settings.api_region(ProviderId::MiniMax), "cn");

    // Usage sources
    assert_eq!(settings.usage_source(ProviderId::Claude), "ccusage");
    assert_eq!(settings.usage_source(ProviderId::Codex), "manual");

    // Codex booleans
    assert!(!settings.openai_web_extras(ProviderId::Codex));
    assert!(settings.historical_tracking(ProviderId::Codex));

    // Claude per-provider boolean
    assert!(settings.avoid_keychain_prompts(ProviderId::Claude));

    // Misc per-provider strings
    assert_eq!(
        settings.workspace_id(ProviderId::OpenCode),
        "ws_placeholder"
    );
    assert_eq!(settings.api_token(ProviderId::MiniMax), "TOK_PLACEHOLDER");
    assert_eq!(settings.ide_base_path(ProviderId::JetBrains), "C:/JB");

    // Legacy field-name aliases agree with typed accessors.
    assert_eq!(settings.codex_cookie_source(), "manual");
    assert_eq!(settings.alibaba_api_region(), "cn");
    assert!(settings.codex_historical_tracking());
    assert!(!settings.codex_openai_web_extras());
    assert!(settings.claude_avoid_keychain_prompts());
}

/// General settings serialization must retain non-secret provider values but
/// omit credentials that belong in the dedicated secure stores.
#[test]
fn test_provider_configs_roundtrip() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Codex, "manual");
    settings.set_cookie_source(ProviderId::Claude, "browser");
    settings.set_usage_source(ProviderId::Claude, "ccusage");
    settings.set_api_region(ProviderId::Alibaba, "cn");
    settings.set_api_region(ProviderId::Zai, "cn");
    settings.set_manual_cookie_header(ProviderId::Amp, "amp=PLACEHOLDER");
    settings.set_api_token(ProviderId::MiniMax, "TOK_PLACEHOLDER");
    settings.set_workspace_id(ProviderId::OpenCode, "ws_placeholder");
    settings.set_ide_base_path(ProviderId::JetBrains, "C:/JB");
    settings.set_openai_web_extras(ProviderId::Codex, false);
    settings.set_historical_tracking(ProviderId::Codex, true);
    settings.set_avoid_keychain_prompts(ProviderId::Claude, true);

    let json = serde_json::to_string(&settings).unwrap();
    // The legacy flat fields must NOT appear in serialized output.
    assert!(!json.contains("\"codex_cookie_source\""), "json: {json}");
    assert!(!json.contains("\"alibaba_api_region\""), "json: {json}");
    assert!(
        !json.contains("\"claude_avoid_keychain_prompts\""),
        "json: {json}"
    );
    assert!(json.contains("\"provider_configs\""), "json: {json}");
    assert!(!json.contains("amp=PLACEHOLDER"), "json: {json}");
    assert!(!json.contains("TOK_PLACEHOLDER"), "json: {json}");
    assert!(!json.contains("manual_cookie_header"), "json: {json}");
    assert!(!json.contains("api_token"), "json: {json}");

    let loaded: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.cookie_source(ProviderId::Codex), "manual");
    assert_eq!(loaded.cookie_source(ProviderId::Claude), "browser");
    assert_eq!(loaded.usage_source(ProviderId::Claude), "ccusage");
    assert_eq!(loaded.api_region(ProviderId::Alibaba), "cn");
    assert_eq!(loaded.api_region(ProviderId::Zai), "cn");
    assert_eq!(loaded.manual_cookie_header(ProviderId::Amp), "");
    assert_eq!(loaded.api_token(ProviderId::MiniMax), "");
    assert_eq!(loaded.workspace_id(ProviderId::OpenCode), "ws_placeholder");
    assert_eq!(loaded.ide_base_path(ProviderId::JetBrains), "C:/JB");
    assert!(!loaded.openai_web_extras(ProviderId::Codex));
    assert!(loaded.historical_tracking(ProviderId::Codex));
    assert!(loaded.avoid_keychain_prompts(ProviderId::Claude));
    assert_eq!(
        loaded.provider_configs.get(&ProviderId::Codex),
        settings.provider_configs.get(&ProviderId::Codex)
    );
}

/// New-format files (no legacy flat fields, only `provider_configs`)
/// must load identically.
#[test]
fn test_new_format_provider_configs_only() {
    let json = r#"{
            "enabled_providers": ["claude"],
            "refresh_interval_secs": 300,
            "provider_configs": {
                "codex": { "cookie_source": "manual", "openai_web_extras": false },
                "alibaba": { "api_region": "cn", "manual_cookie_header": "ali=PLACEHOLDER" }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.cookie_source(ProviderId::Codex), "manual");
    assert!(!settings.openai_web_extras(ProviderId::Codex));
    assert_eq!(settings.api_region(ProviderId::Alibaba), "cn");
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Alibaba),
        "ali=PLACEHOLDER"
    );
    // Untouched providers still get their defaults.
    assert_eq!(settings.cookie_source(ProviderId::Claude), "manual");
    assert_eq!(settings.api_region(ProviderId::Zai), "global");
}

/// Default `Settings` should serialize WITHOUT a `provider_configs`
/// field (empty map skipped).
#[test]
fn test_default_settings_skip_empty_provider_configs() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    assert!(
        !json.contains("\"provider_configs\""),
        "empty map should be skipped, json: {json}"
    );
}

/// Per-provider defaults are applied even when the entry is absent.
#[test]
fn test_per_provider_defaults_applied() {
    let settings = Settings::default();
    assert_eq!(settings.cookie_source(ProviderId::Codex), "manual");
    assert_eq!(settings.usage_source(ProviderId::Codex), "auto");
    assert_eq!(settings.api_region(ProviderId::Alibaba), "singapore");
    assert_eq!(settings.api_region(ProviderId::Zai), "global");
    assert_eq!(settings.api_region(ProviderId::MiniMax), "global");
    assert!(settings.openai_web_extras(ProviderId::Codex));
    assert!(!settings.historical_tracking(ProviderId::Codex));
    assert!(!settings.avoid_keychain_prompts(ProviderId::Claude));
}

/// SBS-1023: the stored flags must actually decide whether keyring I/O runs.
#[test]
fn keychain_access_flags_gate_reads_and_claude_writes() {
    let mut settings = Settings::default();
    assert!(settings.keychain_access_allowed());
    assert!(settings.claude_keychain_access_allowed());

    settings.set_claude_avoid_keychain_prompts(true);
    assert!(settings.keychain_access_allowed());
    assert!(!settings.claude_keychain_access_allowed());

    settings.disable_keychain_access = true;
    settings.set_claude_avoid_keychain_prompts(false);
    assert!(!settings.keychain_access_allowed());
    assert!(!settings.claude_keychain_access_allowed());
}

#[test]
fn codex_spark_usage_visibility_defaults_to_visible_and_roundtrips() {
    let mut settings = Settings::default();
    assert!(settings.codex_spark_usage_visible());

    settings.set_codex_spark_usage_visible(false);
    let serialized = serde_json::to_string(&settings).unwrap();
    let loaded: Settings = serde_json::from_str(&serialized).unwrap();

    assert!(!loaded.codex_spark_usage_visible());
}

// ── Legacy flat-field migration (raw.rs) ─────────────────────────────
//
// `RawSettings` accepts the pre-`provider_configs` on-disk shape and folds it
// into the unified map. A regression here silently discards a saved
// configuration on upgrade, and the user finds out by opening the app to
// defaults. The cases below cover the long-tail providers that had none.

/// Every legacy flat `*_cookie_source` field lands on the right provider.
#[test]
fn legacy_cookie_sources_migrate_for_every_provider() {
    let json = r#"{
            "codex_cookie_source": "manual",
            "claude_cookie_source": "chrome",
            "cursor_cookie_source": "edge",
            "opencode_cookie_source": "firefox",
            "factory_cookie_source": "manual",
            "alibaba_cookie_source": "chrome",
            "kimi_cookie_source": "edge",
            "minimax_cookie_source": "firefox",
            "augment_cookie_source": "manual",
            "amp_cookie_source": "chrome",
            "ollama_cookie_source": "edge"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();

    assert_eq!(settings.cookie_source(ProviderId::Codex), "manual");
    assert_eq!(settings.cookie_source(ProviderId::Claude), "chrome");
    assert_eq!(settings.cookie_source(ProviderId::Cursor), "edge");
    assert_eq!(settings.cookie_source(ProviderId::OpenCode), "firefox");
    assert_eq!(settings.cookie_source(ProviderId::Factory), "manual");
    assert_eq!(settings.cookie_source(ProviderId::Alibaba), "chrome");
    assert_eq!(settings.cookie_source(ProviderId::Kimi), "edge");
    assert_eq!(settings.cookie_source(ProviderId::MiniMax), "firefox");
    assert_eq!(settings.cookie_source(ProviderId::Augment), "manual");
    assert_eq!(settings.cookie_source(ProviderId::Amp), "chrome");
    assert_eq!(settings.cookie_source(ProviderId::Ollama), "edge");
}

/// Every legacy flat manual-cookie-header field lands on the right provider.
/// Kimi is the odd one out: its legacy key is `kimi_manual_cookie_header`,
/// not `kimi_cookie_header`.
#[test]
fn legacy_cookie_headers_migrate_for_every_provider() {
    let json = r#"{
            "alibaba_cookie_header": "ali=PLACEHOLDER",
            "kimi_manual_cookie_header": "kimi=PLACEHOLDER",
            "augment_cookie_header": "aug=PLACEHOLDER",
            "amp_cookie_header": "amp=PLACEHOLDER",
            "ollama_cookie_header": "olm=PLACEHOLDER",
            "minimax_cookie_header": "mm=PLACEHOLDER"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();

    assert_eq!(
        settings.manual_cookie_header(ProviderId::Alibaba),
        "ali=PLACEHOLDER"
    );
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Kimi),
        "kimi=PLACEHOLDER"
    );
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Augment),
        "aug=PLACEHOLDER"
    );
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Amp),
        "amp=PLACEHOLDER"
    );
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Ollama),
        "olm=PLACEHOLDER"
    );
    assert_eq!(
        settings.manual_cookie_header(ProviderId::MiniMax),
        "mm=PLACEHOLDER"
    );
}

/// A file carrying BOTH shapes keeps the modern `provider_configs` value.
/// The legacy field must not overwrite it, otherwise someone who reconfigured
/// a provider in a newer build gets silently reverted to their old value.
#[test]
fn provider_configs_take_precedence_over_legacy_flat_fields() {
    let json = r#"{
            "codex_cookie_source": "manual",
            "claude_usage_source": "auto",
            "alibaba_api_region": "singapore",
            "alibaba_cookie_header": "legacy=lost",
            "opencode_workspace_id": "ws_legacy",
            "minimax_api_token": "tok_legacy",
            "jetbrains_ide_base_path": "C:/legacy",
            "provider_configs": {
                "codex": { "cookie_source": "chrome" },
                "claude": { "usage_source": "web" },
                "alibaba": { "api_region": "cn", "manual_cookie_header": "kept=1" },
                "opencode": { "workspace_id": "ws_kept" },
                "minimax": { "api_token": "tok_kept" },
                "jetbrains": { "ide_base_path": "C:/kept" }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();

    assert_eq!(settings.cookie_source(ProviderId::Codex), "chrome");
    assert_eq!(settings.usage_source(ProviderId::Claude), "web");
    assert_eq!(settings.api_region(ProviderId::Alibaba), "cn");
    assert_eq!(settings.manual_cookie_header(ProviderId::Alibaba), "kept=1");
    assert_eq!(settings.workspace_id(ProviderId::OpenCode), "ws_kept");
    assert_eq!(settings.api_token(ProviderId::MiniMax), "tok_kept");
    assert_eq!(settings.ide_base_path(ProviderId::JetBrains), "C:/kept");
}

/// A legacy field still fills a provider entry that exists but left that
/// particular slot empty. Precedence is per-field, not per-provider.
#[test]
fn legacy_fields_fill_gaps_in_an_existing_provider_entry() {
    // Covers several providers and several slot types. A per-provider (rather
    // than per-field) implementation would drop the legacy value whenever the
    // provider has any entry at all, and testing one provider would only catch
    // that for the provider tested.
    let json = r#"{
            "alibaba_cookie_header": "legacy=used",
            "opencode_workspace_id": "ws_legacy",
            "minimax_api_token": "tok_legacy",
            "codex_usage_source": "web",
            "provider_configs": {
                "alibaba": { "api_region": "cn" },
                "opencode": { "cookie_source": "chrome" },
                "minimax": { "api_region": "cn" },
                "codex": { "cookie_source": "chrome" }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();

    // The modern values survive.
    assert_eq!(settings.api_region(ProviderId::Alibaba), "cn");
    assert_eq!(settings.cookie_source(ProviderId::OpenCode), "chrome");
    assert_eq!(settings.api_region(ProviderId::MiniMax), "cn");
    assert_eq!(settings.cookie_source(ProviderId::Codex), "chrome");

    // Every untouched slot still accepts its legacy value.
    assert_eq!(
        settings.manual_cookie_header(ProviderId::Alibaba),
        "legacy=used"
    );
    assert_eq!(settings.workspace_id(ProviderId::OpenCode), "ws_legacy");
    assert_eq!(settings.api_token(ProviderId::MiniMax), "tok_legacy");
    assert_eq!(settings.usage_source(ProviderId::Codex), "web");
}

/// Only `true` migrates for the two legacy bool flags. A stored `false` must
/// not create an entry, so the per-provider default still applies.
#[test]
fn legacy_false_boolean_flags_do_not_override_defaults() {
    let json = r#"{
            "codex_historical_tracking": false,
            "claude_avoid_keychain_prompts": false
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();

    assert!(!settings.provider_configs.contains_key(&ProviderId::Codex));
    assert!(!settings.provider_configs.contains_key(&ProviderId::Claude));
}

// ── Forward compatibility ────────────────────────────────────────────

/// A config written by a NEWER build must still load on an older one.
///
/// Neither `RawSettings` nor `ProviderConfig` sets `deny_unknown_fields`. Both
/// levels are exercised: a top-level-only test would still pass if the inner
/// `ProviderConfig` started rejecting unknown keys, and downgrades would break
/// while this test stayed green.
#[test]
fn unknown_fields_from_a_newer_build_are_ignored() {
    let json = r#"{
            "refresh_interval_secs": 900,
            "a_field_from_the_future": {"nested": [1, 2, 3]},
            "another_unknown": "ignore me",
            "provider_configs": {
                "codex": {
                    "cookie_source": "chrome",
                    "a_per_provider_field_from_the_future": true,
                    "another_nested_unknown": {"deep": [4, 5]}
                }
            }
        }"#;

    let settings: Settings =
        serde_json::from_str(json).expect("unknown fields must not fail the load");

    assert_eq!(settings.refresh_interval_secs, 900);
    assert_eq!(
        settings.cookie_source(ProviderId::Codex),
        "chrome",
        "a known sibling key must still load past an unknown per-provider field"
    );
}

/// SBS-625: an unknown key in the `provider_configs` MAP is a different failure
/// from an unknown FIELD. `HashMap<ProviderId, _>` made serde reject the entire
/// document, `Settings::load` fell back to defaults, and the next save persisted
/// them over the user's real file.
#[test]
fn an_unknown_provider_configs_key_does_not_reset_the_rest_of_settings() {
    let json = r#"{
            "refresh_interval_secs": 42,
            "enabled_providers": ["claude"],
            "hide_personal_info": true,
            "provider_configs": {
                "codex": { "cookie_source": "manual" },
                "brand_new_provider": { "cookie_source": "chrome" }
            }
        }"#;

    // Exactly what Settings::load does on a parse failure.
    let settings: Settings = serde_json::from_str(json).unwrap_or_default();

    assert_eq!(settings.refresh_interval_secs, 42);
    assert!(settings.enabled_providers.contains("claude"));
    assert_eq!(settings.enabled_providers.len(), 1);
    assert!(settings.hide_personal_info);
    assert_eq!(
        settings.cookie_source(ProviderId::Codex),
        "manual",
        "known provider_configs entries must still load"
    );
}

/// The unknown id itself is parked, not deleted, so a downgrade hands it back
/// intact to the build that understands it.
#[test]
fn an_unknown_provider_configs_key_round_trips_through_a_save() {
    let json = r#"{
            "provider_configs": {
                "codex": { "cookie_source": "manual" },
                "brand_new_provider": { "cookie_source": "chrome" }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).expect("load");
    assert_eq!(
        settings
            .unrecognized_provider_configs
            .get("brand_new_provider")
            .and_then(|config| config.cookie_source.as_deref()),
        Some("chrome")
    );

    // Save (serialize) then load again, as a tray toggle or preference edit
    // would, and the unknown provider must still be there.
    let written = serde_json::to_string(&settings).expect("serialize");
    let reloaded: Settings = serde_json::from_str(&written).expect("reload");
    assert_eq!(
        reloaded
            .unrecognized_provider_configs
            .get("brand_new_provider")
            .and_then(|config| config.cookie_source.as_deref()),
        Some("chrome"),
        "saving must not drop a provider config we merely failed to recognize"
    );
    assert_eq!(reloaded.cookie_source(ProviderId::Codex), "manual");
}

/// A build that later learns the id folds it back into the real map rather than
/// leaving it stranded in the parking field.
#[test]
fn a_parked_provider_config_is_reclaimed_once_the_id_is_known() {
    let json = r#"{
            "unrecognized_provider_configs": {
                "codex": { "cookie_source": "chrome" }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).expect("load");
    assert_eq!(settings.cookie_source(ProviderId::Codex), "chrome");
    assert!(settings.unrecognized_provider_configs.is_empty());
}

/// The same SBS-625 failure reached through a different door: a closed enum
/// whose stored value this build does not know. `#[serde(default)]` covers a
/// MISSING field, not a present-but-unparseable one, so a newer build's
/// `ui_language` used to fail the whole document and reset every preference.
#[test]
fn an_unknown_enum_value_does_not_reset_the_rest_of_settings() {
    let json = r#"{
            "refresh_interval_secs": 42,
            "hide_personal_info": true,
            "ui_language": "klingon",
            "theme": "solarized",
            "update_channel": "nightly"
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap_or_default();

    assert_eq!(
        settings.refresh_interval_secs, 42,
        "an unreadable enum must degrade to its own default, not take its siblings"
    );
    assert!(settings.hide_personal_info);
    // The unreadable fields fall back individually.
    assert_eq!(settings.ui_language, Language::default());
    assert_eq!(settings.theme, ThemePreference::default());
    assert_eq!(settings.update_channel, UpdateChannel::default());
}

/// One unreadable metric preference must not reset every provider's choice.
#[test]
fn an_unknown_provider_metric_drops_only_that_entry() {
    let json = r#"{
            "refresh_interval_secs": 42,
            "provider_metrics": {
                "codex": "weekly",
                "claude": "a_metric_from_the_future"
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap_or_default();

    assert_eq!(settings.refresh_interval_secs, 42);
    assert_eq!(
        settings.provider_metrics.get("codex"),
        Some(&MetricPreference::Weekly)
    );
    assert!(!settings.provider_metrics.contains_key("claude"));
}

/// A legacy inline credential under an id this build cannot resolve must still
/// reach the secure store. `manual_cookie_header` / `api_token` are
/// `skip_serializing`, so leaving it parked would drop it on the next save.
#[test]
fn a_legacy_credential_under_an_unknown_provider_id_is_still_migrated() {
    let json = r#"{
            "provider_configs": {
                "brand_new_provider": {
                    "cookie_source": "manual",
                    "api_token": "future-token"
                }
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).expect("load");
    let parked = settings
        .unrecognized_provider_configs
        .get("brand_new_provider")
        .expect("unknown id is parked");
    assert_eq!(
        parked.api_token.as_deref(),
        Some("future-token"),
        "the inline credential must survive the load so migration can see it"
    );
    assert!(
        carries_legacy_credentials(parked),
        "migration must recognize a parked entry as carrying legacy credentials"
    );
}

/// An empty object is a valid config and yields defaults.
#[test]
fn an_empty_config_object_loads_as_defaults() {
    let settings: Settings = serde_json::from_str("{}").unwrap();
    let defaults = Settings::default();

    // Compare the whole serialized form rather than a hand-picked set of
    // fields. `Settings` has no `PartialEq`, and spot-checking would let a
    // diverging default for any unlisted field through, which is exactly the
    // regression that makes a fresh install come up wrong.
    let mut loaded = serde_json::to_value(&settings).unwrap();
    let mut expected = serde_json::to_value(&defaults).unwrap();

    // `enabled_providers` is a HashSet, so its serialized order is not stable.
    for value in [&mut loaded, &mut expected] {
        let list = value["enabled_providers"]
            .as_array_mut()
            .expect("enabled_providers serializes as an array");
        list.sort_by_key(|entry| entry.as_str().unwrap_or_default().to_string());
    }

    // One field deliberately diverges. `float_bar_contrast` carries meaning in
    // its absence: None means "fall back to the legacy dark_text flag", which
    // `resolved_float_bar_contrast` implements. See
    // legacy_dark_text_setting_is_preserved_when_contrast_is_absent.
    assert_eq!(loaded["float_bar_contrast"], serde_json::Value::Null);
    assert_eq!(expected["float_bar_contrast"], serde_json::json!("auto"));
    loaded["float_bar_contrast"] = serde_json::Value::Null;
    expected["float_bar_contrast"] = serde_json::Value::Null;

    assert_eq!(
        loaded, expected,
        "apart from the one documented exception, an empty config must be \
         indistinguishable from Settings::default()"
    );
}

/// A config predating `float_bar_show_reset_inline` must adopt the default
/// (`true`), not the bool type default (`false`).
///
/// A bare field-level `#[serde(default)]` shadows the container default, which
/// is what previously split behavior: a fresh install showed the inline reset
/// while every upgraded install hid it, on the same build.
#[test]
fn absent_show_reset_inline_adopts_the_default_rather_than_false() {
    let legacy: Settings = serde_json::from_str(
        r#"{
            "enabled_providers": ["claude", "codex"],
            "refresh_interval_secs": 300
        }"#,
    )
    .expect("parse a config written before the field existed");

    assert!(
        legacy.float_bar_show_reset_inline,
        "an absent key must take Settings::default(), not bool::default()"
    );
    assert_eq!(
        legacy.float_bar_show_reset_inline,
        Settings::default().float_bar_show_reset_inline
    );
}

/// An explicit opt-out still survives the load. The default only fills a
/// genuinely absent key, so turning the setting off is not undone on restart.
#[test]
fn explicit_show_reset_inline_false_is_preserved() {
    let opted_out: Settings = serde_json::from_str(r#"{"float_bar_show_reset_inline": false}"#)
        .expect("parse an explicit opt-out");
    assert!(!opted_out.float_bar_show_reset_inline);

    // And it survives a save/load cycle rather than snapping back to true.
    let json = serde_json::to_string(&opted_out).expect("serialize");
    let back: Settings = serde_json::from_str(&json).expect("deserialize");
    assert!(!back.float_bar_show_reset_inline);
}

// ── Taskbar account map sanitization ─────────────────────────────────

/// Provider keys are trimmed and lowercased, values are trimmed but NOT
/// case-folded, and blank entries are dropped so "Auto" stays represented by a
/// missing key.
///
/// The mixed-case account value is deliberate. Account ids are case-sensitive,
/// so lowercasing a value would stop it matching and silently fall the provider
/// back to Auto. An all-lowercase fixture could not detect that.
#[test]
fn taskbar_account_map_is_sanitized_on_load() {
    let json = r#"{
            "taskbar_account_by_provider": {
                "  CODEX  ": "  Work-ID  ",
                "Claude": "Personal.Account@Example.com",
                "cursor": "   ",
                "   ": "orphan-value"
            }
        }"#;

    let settings: Settings = serde_json::from_str(json).unwrap();
    let map = &settings.taskbar_account_by_provider;

    assert_eq!(
        map.get("codex").map(String::as_str),
        Some("Work-ID"),
        "the key is lowercased but the account value must keep its case"
    );
    assert_eq!(
        map.get("claude").map(String::as_str),
        Some("Personal.Account@Example.com")
    );
    assert!(
        !map.contains_key("cursor"),
        "a blank account must be dropped, not stored as empty"
    );
    assert_eq!(map.len(), 2, "blank provider keys must be dropped too");
}

/// SBS-920: the keyring copy is cleared before the file stores, so a hook that
/// fails leaves the credential visible in Preferences.
///
/// The other order emptied every file store first, and the Revoke control is
/// hidden exactly when they all report "no credential". A user whose revoke
/// failed halfway would be left with no way to retry it and a keyring token
/// that still authenticates.
#[test]
fn a_failing_revoke_hook_leaves_the_credential_in_preferences() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keys_path = dir.path().join("api_keys.json");
    let cookies_path = dir.path().join("cookies.json");
    let token_store = crate::core::TokenAccountStore::with_path(dir.path().join("tokens.json"));

    let mut keys = ApiKeys::default();
    keys.set(ProviderId::StepFun.cli_name(), "oasis-token", None);
    keys.save_to(&keys_path).expect("seed api keys");

    let error = revoke_managed_credentials_in(
        ProviderId::StepFun,
        &keys_path,
        &cookies_path,
        &token_store,
        || Err(anyhow::anyhow!("keyring is locked")),
    )
    .expect_err("a hook that cannot clear the keyring must fail the revoke");
    assert!(
        error.to_string().contains("keyring is locked"),
        "got {error}"
    );

    let after = ApiKeys::try_load_from(&keys_path).expect("reload api keys");
    assert!(
        after.has_key(ProviderId::StepFun.cli_name()),
        "the credential must survive so Revoke stays available to retry"
    );
}

/// The successful path still clears every store it owns.
#[test]
fn a_successful_revoke_clears_preferences_after_the_hook() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keys_path = dir.path().join("api_keys.json");
    let cookies_path = dir.path().join("cookies.json");
    let token_store = crate::core::TokenAccountStore::with_path(dir.path().join("tokens.json"));

    let mut keys = ApiKeys::default();
    keys.set(ProviderId::StepFun.cli_name(), "oasis-token", None);
    keys.save_to(&keys_path).expect("seed api keys");

    let mut hook_ran = false;
    revoke_managed_credentials_in(
        ProviderId::StepFun,
        &keys_path,
        &cookies_path,
        &token_store,
        || {
            hook_ran = true;
            Ok(())
        },
    )
    .expect("revoke");

    assert!(hook_ran, "the keyring hook must run");
    let after = ApiKeys::try_load_from(&keys_path).expect("reload api keys");
    assert!(!after.has_key(ProviderId::StepFun.cli_name()));
}

#[test]
fn unparseable_settings_are_moved_aside_instead_of_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = "{\"refresh_interval_secs\":\"60\"";
    std::fs::write(&path, original).expect("write corrupt settings");

    let loaded = Settings::parse_or_quarantine(&path, original);
    assert_eq!(
        loaded.refresh_interval_secs,
        Settings::default().refresh_interval_secs
    );
    assert!(
        !path.exists(),
        "the live path must be vacated so a later save cannot clobber the original"
    );
    let backup = Settings::backup_path(&path);
    assert_eq!(
        std::fs::read_to_string(&backup).expect("read backup"),
        original
    );
}

/// SBS-1029: `Settings::load` used to call [`Settings::parse_or_quarantine`]
/// without the state lock. A concurrent locked [`Settings::try_update`] can
/// write a good repair after this thread read the corrupt bytes; renaming
/// then would move that repair to `.bak`.
#[test]
fn unlocked_corrupt_read_does_not_rename_a_concurrent_repair() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = "{\"refresh_interval_secs\":\"60\"";
    std::fs::write(&path, original).expect("write corrupt settings");

    let (loaded, pending_quarantine) = Settings::read_path(&path, false);
    assert!(
        pending_quarantine,
        "an unlocked parse failure must ask load() to retry under the lock"
    );
    assert_eq!(
        loaded.refresh_interval_secs,
        Settings::default().refresh_interval_secs
    );
    assert!(
        path.exists(),
        "the unlocked path must leave the live file in place"
    );
    assert!(
        !Settings::backup_path(&path).exists(),
        "the unlocked path must not create a backup"
    );

    // Locked try_update repair occupies the live path (SBS-954 / SBS-1029).
    let repaired = "{\n  \"refresh_interval_secs\": 42\n}";
    std::fs::write(&path, repaired).expect("write repair");

    // load() re-reads under the lock. The file is valid now, so quarantine
    // must not run and the repair must stay.
    let (reloaded, pending_after_repair) = Settings::read_path(&path, true);
    assert!(
        !pending_after_repair,
        "a successful locked re-read has nothing left to quarantine"
    );
    assert_eq!(reloaded.refresh_interval_secs, 42);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read live"),
        repaired,
        "the repair must stay at the live path"
    );
    assert!(
        !Settings::backup_path(&path).exists(),
        "a good repair must not be moved to .bak"
    );
}

/// SBS-1074: `read_string` errors used to become defaults with no `.bak`, so
/// the next [`Settings::try_update`] atomically replaced the live undecodable
/// file. Parse failures already quarantine (SBS-954); treat a read failure
/// on an existing file the same way.
fn unsupported_protected_settings() -> String {
    serde_json::json!({
        "format": "codexbar.secure-file",
        "version": 99,
        "protection": "windows-dpapi-user",
        "payload": "AAAA",
    })
    .to_string()
}

fn dpapi_protected_settings() -> String {
    serde_json::json!({
        "format": "codexbar.secure-file",
        "version": 1,
        "protection": "windows-dpapi-user",
        "payload": "bm90LXJlYWwtZHBhcGk=",
    })
    .to_string()
}

/// The read-modify-write `try_update` takes: load under the lock (quarantine
/// allowed), then atomically replace the live path with the in-memory
/// snapshot. Tests use this instead of [`Settings::try_update`] so they do
/// not touch the process config dir.
fn try_update_write_at(path: &std::path::Path) {
    let (settings, _) = Settings::read_path(path, true);
    let json = serde_json::to_string_pretty(&settings).expect("serialize defaults");
    crate::secure_file::write_string(path, &json).expect("try_update write");
}

fn assert_read_failure_is_pending_when_unlocked(path: &std::path::Path, original: &[u8]) {
    let (loaded, pending_quarantine) = Settings::read_path(path, false);
    assert!(
        pending_quarantine,
        "an unlocked read failure must ask load() to retry under the lock"
    );
    assert_eq!(
        loaded.refresh_interval_secs,
        Settings::default().refresh_interval_secs
    );
    assert_eq!(
        std::fs::read(path).expect("live file"),
        original,
        "the unlocked path must leave the live file in place"
    );
    assert!(
        !Settings::backup_path(path).exists(),
        "the unlocked path must not create a backup"
    );
}

fn assert_read_failure_quarantines_when_locked(path: &std::path::Path, original: &[u8]) {
    let (loaded, pending_quarantine) = Settings::read_path(path, true);
    assert!(
        !pending_quarantine,
        "a locked read failure quarantines immediately"
    );
    assert_eq!(
        loaded.refresh_interval_secs,
        Settings::default().refresh_interval_secs
    );
    assert!(
        !path.exists(),
        "the live path must be vacated so a later save cannot clobber the original"
    );
    assert_eq!(
        std::fs::read(Settings::backup_path(path)).expect("read backup"),
        original
    );
}

#[test]
fn unsupported_secure_file_version_is_not_treated_as_empty_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = unsupported_protected_settings();
    std::fs::write(&path, &original).expect("write unsupported ProtectedFile");

    crate::secure_file::read_string(&path).expect_err("fixture must fail read_string");
    assert_read_failure_is_pending_when_unlocked(&path, original.as_bytes());
    assert_read_failure_quarantines_when_locked(&path, original.as_bytes());
}

#[test]
fn dpapi_unprotect_failure_is_not_treated_as_empty_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = dpapi_protected_settings();
    std::fs::write(&path, &original).expect("write undecryptable ProtectedFile");

    crate::secure_file::read_string(&path).expect_err("fixture must fail read_string");
    assert_read_failure_is_pending_when_unlocked(&path, original.as_bytes());
    assert_read_failure_quarantines_when_locked(&path, original.as_bytes());
}

#[test]
fn io_failure_on_existing_settings_is_not_treated_as_empty_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = b"\xff\xfe{not-utf8".to_vec();
    std::fs::write(&path, &original).expect("write invalid UTF-8");

    crate::secure_file::read_string(&path).expect_err("fixture must fail read_string");
    assert_read_failure_is_pending_when_unlocked(&path, &original);
    assert_read_failure_quarantines_when_locked(&path, &original);
}

/// Without the SBS-1074 quarantine, this write replaces the live undecodable
/// bytes and leaves no `.bak`.
#[test]
fn try_update_does_not_wipe_an_undecodable_settings_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    let original = unsupported_protected_settings();
    std::fs::write(&path, &original).expect("write unsupported ProtectedFile");

    try_update_write_at(&path);

    assert_eq!(
        std::fs::read_to_string(Settings::backup_path(&path)).expect("quarantined original"),
        original,
        "try_update must move the undecodable file aside instead of overwriting it"
    );
    let live = crate::secure_file::read_string(&path).expect("defaults written to vacated path");
    let written: Settings = serde_json::from_str(&live).expect("written settings parse");
    assert_eq!(
        written.refresh_interval_secs,
        Settings::default().refresh_interval_secs
    );
}

/// SBS-964: a privacy-conscious reader who leaves incident badges off still
/// sees models.dev and GitHub traffic. Claiming the badge is the only
/// non-provider outbound request is false; the copy has to name those hosts.
#[test]
fn incident_badge_copy_does_not_claim_it_is_the_only_non_provider_outbound() {
    const SURFACES: &[(&str, &str)] = &[
        ("rust/src/settings.rs", include_str!("../settings.rs")),
        ("CHANGELOG.md", include_str!("../../../CHANGELOG.md")),
        (
            "docs/DATA_SOURCES.md",
            include_str!("../../../docs/DATA_SOURCES.md"),
        ),
        (
            "apps/desktop-tauri/src-tauri/src/provider_incidents.rs",
            include_str!("../../../apps/desktop-tauri/src-tauri/src/provider_incidents.rs"),
        ),
        (
            "site/privacy/index.html",
            include_str!("../../../site/privacy/index.html"),
        ),
        (
            "rust/src/locale/en-US.ftl",
            include_str!("../locale/en-US.ftl"),
        ),
        (
            "rust/src/locale/zh-CN.ftl",
            include_str!("../locale/zh-CN.ftl"),
        ),
        ("site/index.html", include_str!("../../../site/index.html")),
        (
            "docs/SETTINGS_JSON.md",
            include_str!("../../../docs/SETTINGS_JSON.md"),
        ),
    ];

    // Every one of these is false. Enabling a provider already schedules usage
    // HTTP to that provider with badges off, so "tied to your provider list"
    // is no more exclusive than "not to a provider" was - it just moves the
    // false claim rather than dropping it (SBS-964).
    const EXCLUSIVE: &[&str] = &[
        "the only outbound request Ceiling makes that is not to a provider",
        "the only outbound request Ceiling makes that you have not already signed in to",
        "The only non-provider destination is",
        "the only outbound request tied to",
        "is the only outbound request",
    ];

    for (name, text) in SURFACES {
        for phrase in EXCLUSIVE {
            assert!(
                !text.contains(phrase),
                "{name} still claims a unique non-provider outbound that the code does not have"
            );
        }
    }

    let settings = include_str!("../settings.rs");
    let field = settings
        .find("pub provider_incident_badges_enabled")
        .expect("incident badge field");
    let field_docs = &settings[field.saturating_sub(500)..field];
    assert!(
        field_docs.contains("models.dev"),
        "settings field docs must name models.dev"
    );
    assert!(
        field_docs.contains("GitHub"),
        "settings field docs must name the GitHub update check"
    );

    let changelog = include_str!("../../../CHANGELOG.md");
    let badge_start = changelog
        .find("**Providers having a public outage get a badge")
        .expect("1.5.33 incident badge bullet");
    let after = &changelog[badge_start..];
    let badge_end = after.find("\n- **").expect("next changelog bullet");
    let badge_bullet = &after[..badge_end];
    assert!(
        badge_bullet.contains("public status page"),
        "1.5.33 badge notes must say what the badge actually adds"
    );
    assert!(
        badge_bullet.contains("models.dev"),
        "1.5.33 badge notes must name models.dev"
    );
    assert!(
        badge_bullet.contains("GitHub"),
        "1.5.33 badge notes must name the GitHub update check"
    );

    let incidents = include_str!("../../../apps/desktop-tauri/src-tauri/src/provider_incidents.rs");
    let module_docs = &incidents[..incidents.find("use std::").expect("module docs")];
    assert!(
        module_docs.contains("models.dev"),
        "incident badge module docs must name models.dev"
    );
    assert!(
        module_docs.contains("GitHub"),
        "incident badge module docs must name the GitHub update check"
    );

    let policy = include_str!("../../../docs/DATA_SOURCES.md")
        .split("## Network policy")
        .nth(1)
        .expect("network policy");
    assert!(
        policy.contains("models.dev"),
        "DATA_SOURCES network policy must name models.dev"
    );
    assert!(
        policy.contains("GitHub") || policy.contains("api.github.com"),
        "DATA_SOURCES network policy must name the GitHub update check"
    );

    let privacy = include_str!("../../../site/privacy/index.html");
    assert!(
        privacy.contains("models.dev"),
        "privacy page must name models.dev"
    );
    // Not a bare "GitHub": that word already appears in the analytics
    // paragraph, the third-party paragraph, the issue-tracker link, and the
    // footer, so the disclosure could be deleted with this test still green.
    assert!(
        privacy.contains("GitHub for update checks"),
        "privacy page must name GitHub as the update-check host, in those words"
    );
}
