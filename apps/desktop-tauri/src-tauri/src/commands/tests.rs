use std::collections::HashMap;

use super::{
    NamedRateWindowSnapshot, ProviderSummary, ProviderUsageSnapshot, provider_cookie_source_lookup,
    provider_region_lookup, validate_external_url, validate_surface_target,
};
use crate::surface::SurfaceMode;
use crate::surface_target::SurfaceTarget;
use codexbar::core::{
    ConfiguredAccounts, FetchContext, ProviderAccountData, ProviderFetchResult, ProviderId,
    SourceMode, TokenAccount, instantiate_provider,
};
use codexbar::host::session::launch_block_reason;
use codexbar::settings::{ApiKeys, ManualCookies, Settings};

#[test]
fn validate_surface_target_accepts_matching_target() {
    let target = validate_surface_target(
        SurfaceMode::Settings,
        SurfaceTarget::Settings {
            tab: "accounts".into(),
        },
    )
    .unwrap();

    assert_eq!(
        target,
        SurfaceTarget::Settings {
            tab: "accounts".into()
        }
    );
}

#[test]
fn validate_surface_target_rejects_mismatched_target() {
    let error = validate_surface_target(
        SurfaceMode::TrayPanel,
        SurfaceTarget::Settings {
            tab: "accounts".into(),
        },
    )
    .unwrap_err();

    assert!(error.contains("not valid for mode 'trayPanel'"));
}

#[test]
fn validate_surface_target_rejects_hidden_mode() {
    let error = validate_surface_target(SurfaceMode::Hidden, SurfaceTarget::Summary).unwrap_err();

    assert!(error.contains("only supports visible surfaces"));
}

#[test]
fn external_url_validation_allows_only_http_urls() {
    assert_eq!(
        validate_external_url(" https://github.com/Finesssee/Win-CodexBar ").unwrap(),
        "https://github.com/Finesssee/Win-CodexBar"
    );
    assert_eq!(
        validate_external_url("http://codexbar.app").unwrap(),
        "http://codexbar.app"
    );

    for invalid in [
        "",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://bad\nhost",
    ] {
        assert!(
            validate_external_url(invalid).is_err(),
            "accepted invalid URL: {invalid:?}"
        );
    }
}

#[test]
fn credential_status_labels_do_not_include_error_details() {
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Missing),
        "missing"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Plaintext),
        "plaintext"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Protected(
            "windows-dpapi-user".to_string(),
        )),
        "protected:windows-dpapi-user"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Unreadable(
            "secret path / token".to_string(),
        )),
        "unreadable"
    );
}

#[test]
fn credential_status_is_scoped_to_the_provider() {
    let mut api_keys = ApiKeys::default();
    api_keys.set("claude", "sk-claude", None);
    let mut manual_cookies = ManualCookies::default();
    manual_cookies.set("claude", "sessionKey=secret");
    let mut account_data = ProviderAccountData::new();
    let token_account = TokenAccount::new("Claude", "token-secret");
    let token_account_id = token_account.id.to_string();
    let detail_account_id = "00000000-0000-0000-0000-000000000000";
    assert_ne!(token_account_id, detail_account_id);
    account_data.add_account(token_account);
    let mut token_accounts = HashMap::new();
    token_accounts.insert(ProviderId::Claude, account_data);

    let protected =
        || codexbar::secure_file::SecureFileStatus::Protected("windows-dpapi-user".to_string());
    // Detail-pane account tabs use a separate UUID namespace from token
    // accounts. The detail account ID is intentionally not an input here:
    // credential presence is provider-level.
    let claude = super::build_credential_storage_status(
        ProviderId::Claude,
        super::CredentialStorageSources {
            manual_cookie_file_status: Some(protected()),
            manual_cookies: Some(&manual_cookies),
            api_key_file_status: Some(protected()),
            api_keys: Some(&api_keys),
            token_account_file_status: protected(),
            token_accounts: Some(&token_accounts),
        },
    );
    assert_eq!(claude.api_keys.has_provider_credentials, Some(true));
    assert_eq!(claude.manual_cookies.has_provider_credentials, Some(true));
    assert_eq!(claude.token_accounts.has_provider_credentials, Some(true));

    // The files remain globally protected, but another provider has no entry
    // in any of them and must not inherit Claude's status in its detail pane.
    let codex = super::build_credential_storage_status(
        ProviderId::Codex,
        super::CredentialStorageSources {
            manual_cookie_file_status: Some(protected()),
            manual_cookies: Some(&manual_cookies),
            api_key_file_status: Some(protected()),
            api_keys: Some(&api_keys),
            token_account_file_status: protected(),
            token_accounts: Some(&token_accounts),
        },
    );
    assert_eq!(codex.api_keys.file_status, "protected:windows-dpapi-user");
    assert_eq!(codex.api_keys.has_provider_credentials, Some(false));
    assert_eq!(codex.manual_cookies.has_provider_credentials, Some(false));
    assert_eq!(codex.token_accounts.has_provider_credentials, Some(false));

    api_keys.remove("claude");
    manual_cookies.remove("claude");
    token_accounts.remove(&ProviderId::Claude);
    let revoked = super::build_credential_storage_status(
        ProviderId::Claude,
        super::CredentialStorageSources {
            manual_cookie_file_status: Some(protected()),
            manual_cookies: Some(&manual_cookies),
            api_key_file_status: Some(protected()),
            api_keys: Some(&api_keys),
            token_account_file_status: protected(),
            token_accounts: Some(&token_accounts),
        },
    );
    assert_eq!(revoked.api_keys.has_provider_credentials, Some(false));
    assert_eq!(revoked.manual_cookies.has_provider_credentials, Some(false));
    assert_eq!(revoked.token_accounts.has_provider_credentials, Some(false));
}

#[test]
fn unreadable_credential_store_does_not_claim_provider_absence() {
    let status = super::build_credential_storage_status(
        ProviderId::Claude,
        super::CredentialStorageSources {
            manual_cookie_file_status: Some(codexbar::secure_file::SecureFileStatus::Unreadable(
                "secret error".to_string(),
            )),
            manual_cookies: None,
            api_key_file_status: Some(codexbar::secure_file::SecureFileStatus::Missing),
            api_keys: Some(&ApiKeys::default()),
            token_account_file_status: codexbar::secure_file::SecureFileStatus::Missing,
            token_accounts: Some(&HashMap::new()),
        },
    );

    assert_eq!(status.manual_cookies.file_status, "unreadable");
    assert_eq!(status.manual_cookies.has_provider_credentials, None);
    assert_eq!(status.api_keys.has_provider_credentials, Some(false));
    assert_eq!(status.token_accounts.has_provider_credentials, Some(false));
}

#[test]
fn command_inputs_reject_invalid_provider_ids_before_storage_writes() {
    assert!(super::set_api_key("not-a-provider".into(), "sk-test".into(), None).is_err());
    assert!(super::set_manual_cookie("not-a-provider".into(), "a=b".into()).is_err());
    assert!(super::remove_api_key("bad\nprovider".into()).is_err());
    assert!(super::remove_manual_cookie("".into()).is_err());
    assert!(super::get_credential_storage_status("not-a-provider".into()).is_err());
}

#[test]
fn command_inputs_reject_multiline_secrets() {
    assert!(super::set_api_key("openrouter".into(), "sk-test\nnext".into(), None).is_err());
    assert!(super::set_manual_cookie("codex".into(), "a=b\nc=d".into()).is_err());
}

#[test]
fn command_inputs_reject_unknown_region_values() {
    assert!(super::set_provider_region("zai".into(), "moon".into()).is_err());
}

#[test]
fn apply_provider_order_dedupes_and_appends_unknown_canonical() {
    // Request only "codex" and "claude" — remaining canonical ids should
    // be appended after, preserving canonical order.
    let order =
        codexbar::settings::normalize_provider_order(&["codex".to_string(), "claude".to_string()]);
    assert_eq!(order[0], "codex");
    assert_eq!(order[1], "claude");
    // Every canonical id appears exactly once.
    let mut sorted = order.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), order.len());
    // Every canonical id is present.
    let canonical = codexbar::core::ProviderId::all()
        .iter()
        .map(|p| p.cli_name().to_string())
        .collect::<Vec<_>>();
    for id in &canonical {
        assert!(order.contains(id), "missing canonical id: {id}");
    }
}

#[test]
fn apply_provider_order_ignores_unknown_ids() {
    let order = codexbar::settings::normalize_provider_order(&[
        "not-a-provider".to_string(),
        "codex".to_string(),
    ]);
    assert_eq!(order[0], "codex");
    assert!(!order.iter().any(|id| id == "not-a-provider"));
}

#[test]
fn provider_summaries_reflect_settings_order() {
    let canonical_len = codexbar::core::ProviderId::all().len();
    let s = Settings::default();
    let summaries: Vec<ProviderSummary> = super::build_provider_summaries(&s);
    assert_eq!(summaries.len(), canonical_len);
    // Index is assigned in emission order.
    for (i, s) in summaries.iter().enumerate() {
        assert_eq!(s.order, i as u32);
    }
}

#[test]
fn provider_catalog_preserves_partial_config_order() {
    let settings = Settings {
        provider_order: codexbar::settings::normalize_provider_order(&[
            "gemini".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ]),
        ..Settings::default()
    };

    let catalog = super::provider_catalog_for(&settings);

    assert_eq!(
        catalog
            .iter()
            .take(3)
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"]
    );
}

#[test]
fn settings_snapshot_preserves_partial_config_order_for_enabled_providers() {
    let settings = Settings {
        enabled_providers: ["gemini", "claude", "codex"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        provider_order: codexbar::settings::normalize_provider_order(&[
            "gemini".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ]),
        ..Settings::default()
    };

    let snapshot = serde_json::to_value(super::SettingsSnapshot::from(settings)).unwrap();

    assert_eq!(
        snapshot["providerOrder"]
            .as_array()
            .unwrap()
            .iter()
            .take(3)
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"],
    );
    assert_eq!(
        snapshot["enabledProviders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"],
    );
}

#[test]
fn provider_cookie_source_lookup_roundtrips_known_providers() {
    let mut s = Settings::default();
    s.set_cookie_source(ProviderId::Codex, "cli-config");
    assert_eq!(
        provider_cookie_source_lookup(&s, "codex").as_deref(),
        Some("cli-config")
    );
    assert!(provider_cookie_source_lookup(&s, "unknown-provider").is_none());
}

#[test]
fn provider_region_lookup_roundtrips_known_providers() {
    let mut s = Settings::default();
    super::provider_region_set(&mut s, "alibaba", "china".to_string()).unwrap();
    assert_eq!(
        provider_region_lookup(&s, "alibaba").as_deref(),
        Some("china")
    );
    // Non-regional providers return None.
    assert!(provider_region_lookup(&s, "claude").is_none());
}

#[test]
fn minimax_region_lookup_normalizes_legacy_china_value() {
    let mut s = Settings::default();
    super::provider_region_set(&mut s, "minimax", "china".to_string()).unwrap();
    assert_eq!(provider_region_lookup(&s, "minimax").as_deref(), Some("cn"));
}

#[test]
fn fetch_context_defaults_to_manual_cookies_without_browser_import() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    // Cursor defaults to Automatic and resolves the signed-in IDE session in-provider.
    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_claude_uses_oauth_without_manual_cookie() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_grok_uses_auto_without_manual_cookie() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Grok,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    // Default cookie source is Manual with no paste; Grok should fall back to
    // local ~/.grok/auth.json via Auto (not Cli-only, which previously failed).
    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_grok_cookie_off_uses_auto() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Grok, "off");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Grok,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_claude_explicit_cli_source_still_uses_cli() {
    let mut settings = Settings::default();
    settings.set_usage_source(ProviderId::Claude, "cli");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Cli);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_manual_cookie_uses_web_without_browser_import() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Cursor, "manual");
    let mut cookies = ManualCookies::default();
    cookies.set("cursor", "session=abc123");
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(ctx.manual_cookie_header.as_deref(), Some("session=abc123"));
}

#[test]
fn fetch_context_api_key_provider_uses_auto_without_cookie_import() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("deepseek", "sk-test", None);
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::DeepSeek,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-test"));
}

#[test]
fn fetch_context_kimi_api_key_preserves_auto_for_web_fallback() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("kimi", "sk-kimi-test", None);
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Kimi,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-kimi-test"));
}

#[test]
fn fetch_context_includes_minimax_region() {
    let mut settings = Settings::default();
    settings.set_api_region(ProviderId::MiniMax, "cn");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::MiniMax,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.api_region.as_deref(), Some("cn"));
}

#[test]
fn fetch_context_token_account_uses_web_cookie_header() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Work", "abc123"));
    token_accounts.insert(ProviderId::Ollama, data);

    let ctx = super::build_fetch_context(
        ProviderId::Ollama,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("__Secure-session=abc123")
    );
}

#[test]
fn fetch_context_claude_oauth_token_account_uses_oauth() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Claude OAuth", "sk-ant-oat01-abc123"));
    token_accounts.insert(ProviderId::Claude, data);

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-ant-oat01-abc123"));
}

#[test]
fn fetch_context_copilot_token_account_uses_oauth_api_key() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("GitHub", "gho_testtoken"));
    token_accounts.insert(ProviderId::Copilot, data);

    let ctx = super::build_fetch_context(
        ProviderId::Copilot,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("gho_testtoken"));
}

#[test]
fn configured_claude_accounts_do_not_inherit_the_global_session_cookie() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Claude, "manual");
    let mut cookies = ManualCookies::default();
    cookies.set("claude", "sessionKey=sk-ant-global");
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let base_ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );
    assert_eq!(base_ctx.source_mode, SourceMode::Web);
    assert_eq!(
        base_ctx.manual_cookie_header.as_deref(),
        Some("sessionKey=sk-ant-global")
    );

    let mut accounts = ConfiguredAccounts::default();
    accounts
        .claude
        .add_account(codexbar::core::DirectoryAccount::<
            codexbar::core::ClaudeIdentity,
        >::new(
            Some("personal".into()),
            std::path::PathBuf::from("/dirs/personal"),
        ));
    accounts
        .claude
        .add_account(codexbar::core::DirectoryAccount::<
            codexbar::core::ClaudeIdentity,
        >::new(
            Some("work".into()),
            std::path::PathBuf::from("/dirs/work"),
        ));

    let fetches = super::account_fetches(ProviderId::Claude, base_ctx, &accounts);
    assert_eq!(fetches.len(), 2);
    for (ctx, badge) in &fetches {
        assert!(
            ctx.manual_cookie_header.is_none(),
            "account {} inherited the global sessionKey",
            badge.label.as_deref().unwrap_or("?")
        );
        assert!(ctx.account_config_dir.is_some());
    }
    let dirs: Vec<_> = fetches
        .iter()
        .filter_map(|(ctx, _)| ctx.account_config_dir.clone())
        .collect();
    assert!(dirs.contains(&std::path::PathBuf::from("/dirs/personal")));
    assert!(dirs.contains(&std::path::PathBuf::from("/dirs/work")));
}

#[test]
fn ambient_claude_fetch_keeps_the_manual_session_cookie() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Claude, "manual");
    let mut cookies = ManualCookies::default();
    cookies.set("claude", "sessionKey=sk-ant-global");
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let base_ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );
    let fetches =
        super::account_fetches(ProviderId::Claude, base_ctx, &ConfiguredAccounts::default());

    assert_eq!(fetches.len(), 1);
    assert_eq!(
        fetches[0].0.manual_cookie_header.as_deref(),
        Some("sessionKey=sk-ant-global")
    );
    assert!(fetches[0].0.account_config_dir.is_none());
}

#[test]
fn fetch_context_claude_session_token_account_uses_web_cookie() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Claude Web", "sessionKey=abc123"));
    token_accounts.insert(ProviderId::Claude, data);

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("sessionKey=abc123")
    );
    assert!(ctx.api_key.is_none());
}

#[test]
fn fetch_context_token_account_takes_precedence_over_manual_cookie() {
    // Manual-cookie mode: this scenario is specifically about a token-account
    // cookie overriding a manually-entered one, so pin the source explicitly
    // rather than depending on Cursor's default cookie source.
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Cursor, "manual");
    let mut cookies = ManualCookies::default();
    cookies.set("cursor", "manual=old");
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Work", "WorkosCursorSessionToken=new"));
    token_accounts.insert(ProviderId::Cursor, data);

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("WorkosCursorSessionToken=new")
    );
}

#[test]
fn provider_region_set_rejects_non_regional_provider() {
    let mut s = Settings::default();
    let err = super::provider_region_set(&mut s, "claude", "global".into()).unwrap_err();
    assert!(err.contains("claude"));
}

#[test]
fn launch_block_reason_helper_returns_none_when_not_blocked() {
    assert!(launch_block_reason(false, false).is_none());
}

#[test]
fn launch_block_reason_helper_prefers_ssh() {
    let msg = launch_block_reason(true, true).unwrap();
    assert!(msg.contains("SSH"));
}

// ── Phase 6b — provider detail pane ────────────────────────────

#[test]
fn build_provider_detail_populates_identity_urls() {
    let detail = super::build_provider_detail("claude").expect("known provider");
    assert_eq!(detail.id, "claude");
    assert_eq!(detail.display_name, "Claude");
    // Claude advertises a status page URL in its metadata.
    assert!(detail.status_page_url.is_some());
    // No snapshot yet — empty usage bars and no error.
    assert!(detail.session.is_none());
    assert!(detail.last_error.is_none());
    assert!(!detail.has_snapshot);
}

#[test]
fn build_provider_detail_rejects_unknown_provider() {
    let err = super::build_provider_detail("not-a-provider").unwrap_err();
    assert!(err.contains("not-a-provider"));
}

#[test]
fn provider_detail_roundtrips_through_serde() {
    let detail = super::build_provider_detail("codex").expect("known provider");
    let json = serde_json::to_string(&detail).expect("serialize");
    // camelCase rename survives the round-trip.
    assert!(json.contains("\"displayName\""));
    assert!(json.contains("\"hasSnapshot\""));
    assert!(json.contains("\"statusPageUrl\""));
}

#[test]
fn pace_stage_serializes_to_snake_case_string() {
    use codexbar::core::PaceStage;
    assert_eq!(super::pace_stage_str(PaceStage::OnTrack), "on_track");
    assert_eq!(
        super::pace_stage_str(PaceStage::SlightlyAhead),
        "slightly_ahead"
    );
    assert_eq!(super::pace_stage_str(PaceStage::FarAhead), "far_ahead");
    assert_eq!(
        super::pace_stage_str(PaceStage::SlightlyBehind),
        "slightly_behind"
    );
    assert_eq!(super::pace_stage_str(PaceStage::Behind), "behind");
    assert_eq!(super::pace_stage_str(PaceStage::FarBehind), "far_behind");
}

#[test]
fn provider_cache_is_fresh_inside_stale_window() {
    assert!(super::is_provider_cache_fresh(
        Some(std::time::Instant::now()),
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_cache_is_stale_when_missing_timestamp() {
    assert!(!super::is_provider_cache_fresh(
        None,
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_cache_is_stale_after_window() {
    assert!(!super::is_provider_cache_fresh(
        Some(std::time::Instant::now() - std::time::Duration::from_secs(31)),
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_fetch_timeout_allows_slower_authenticated_providers() {
    let ctx = FetchContext {
        web_timeout: 30,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Claude, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Codex, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Copilot, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::DeepSeek, &ctx),
        std::time::Duration::from_secs(35)
    );
}

#[test]
fn provider_fetch_timeout_respects_context_web_timeout_with_cap() {
    let ctx = FetchContext {
        web_timeout: 60,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::T3Chat, &ctx),
        std::time::Duration::from_secs(65)
    );

    let ctx = FetchContext {
        web_timeout: 120,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::AzureOpenAI, &ctx),
        std::time::Duration::from_secs(65)
    );
}

#[test]
fn provider_cache_upsert_replaces_existing_provider() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let mut first = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    let mut second = first.clone();
    first.error = Some("old".to_string());
    second.error = Some("new".to_string());

    let mut cache = vec![first];
    super::upsert_provider_cache(&mut cache, second);

    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].provider_id, "codex");
    assert_eq!(cache[0].error.as_deref(), Some("new"));
}

fn account_snapshot(account_id: &str, used: f64) -> ProviderUsageSnapshot {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(used)),
        cost: None,
        wayfinder_usage: None,
        source_label: "oauth".to_string(),
    };
    let mut snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    snapshot.account_id = Some(account_id.to_string());
    snapshot
}

#[test]
fn the_taskbar_strip_shows_the_account_closest_to_its_limit() {
    let cached = vec![
        account_snapshot("acct-personal", 12.0),
        account_snapshot("acct-work", 91.0),
    ];

    let chosen = super::most_constrained_per_provider(&cached);

    // One entry per provider, and it is the seat about to run out. Emitting
    // both would put two entries under one ProviderId, which is ambiguous for
    // everything downstream.
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].account_id.as_deref(), Some("acct-work"));
}

#[test]
fn the_taskbar_strip_does_not_flicker_between_tied_accounts() {
    let one = vec![
        account_snapshot("acct-b", 50.0),
        account_snapshot("acct-a", 50.0),
    ];
    let other = vec![
        account_snapshot("acct-a", 50.0),
        account_snapshot("acct-b", 50.0),
    ];

    // Readings land in whatever order they finish, which must not change what
    // the strip shows.
    assert_eq!(
        super::most_constrained_per_provider(&one)[0].account_id,
        super::most_constrained_per_provider(&other)[0].account_id
    );
}

fn cursor_account_snapshot(
    account_id: &str,
    plan: f64,
    auto: f64,
    api: Option<f64>,
) -> ProviderUsageSnapshot {
    let metadata = instantiate_provider(ProviderId::Cursor).metadata().clone();
    let mut usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(plan))
        .with_secondary(codexbar::core::RateWindow::new(auto));
    if let Some(api) = api {
        usage =
            usage.with_extra_rate_window("cursor-api", "API", codexbar::core::RateWindow::new(api));
    }
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "oauth".to_string(),
    };
    let mut snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Cursor, &metadata, &result);
    snapshot.account_id = Some(account_id.to_string());
    snapshot
}

/// SBS-1076: picking by Plan used% chose the seat whose blend was hotter even
/// when Auto still had room. The strip ranks Auto/API, so the seat picker must.
#[test]
fn cursor_seat_picker_uses_strip_window_not_plan() {
    let cached = vec![
        cursor_account_snapshot("acct-plan-hot", 95.0, 10.0, Some(8.0)),
        cursor_account_snapshot("acct-auto-hot", 40.0, 70.0, Some(20.0)),
    ];

    let chosen = super::most_constrained_per_provider(&cached);
    assert_eq!(chosen.len(), 1);
    assert_eq!(
        chosen[0].account_id.as_deref(),
        Some("acct-auto-hot"),
        "hotter Auto must win over hotter Plan"
    );
}

#[test]
fn cursor_seat_picker_prefers_api_room_over_exhausted_auto_on_other_seat() {
    let cached = vec![
        cursor_account_snapshot("acct-auto-maxed", 40.0, 100.0, Some(15.0)),
        cursor_account_snapshot("acct-auto-open", 90.0, 20.0, Some(100.0)),
    ];

    let chosen = super::most_constrained_per_provider(&cached);
    assert_eq!(
        chosen[0].account_id.as_deref(),
        Some("acct-auto-open"),
        "Auto with room (20%) is the strip window; the other seat's API 15% is cooler"
    );
}

#[test]
fn the_taskbar_strip_skips_providers_that_failed_to_fetch() {
    let mut errored = account_snapshot("acct-work", 91.0);
    errored.error = Some("offline".to_string());

    let cached = vec![errored];
    let chosen = super::most_constrained_per_provider(&cached);

    assert!(chosen.is_empty());
}

#[test]
fn provider_cache_keeps_one_row_per_account() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "oauth".to_string(),
    };
    let base = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);

    let mut personal = base.clone();
    personal.account_id = Some("acct-personal".to_string());
    personal.account_label = Some("tsouth2@example.com (prolite)".to_string());
    let mut work = base.clone();
    work.account_id = Some("acct-work".to_string());
    work.account_label = Some("work@example.com (team)".to_string());

    let mut cache = Vec::new();
    super::upsert_provider_cache(&mut cache, personal);
    super::upsert_provider_cache(&mut cache, work);

    // Two accounts on one provider must sit side by side. Keying the cache on
    // provider alone made the second silently replace the first.
    assert_eq!(cache.len(), 2, "second account replaced the first");
    let labels: Vec<_> = cache
        .iter()
        .filter_map(|snap| snap.account_label.as_deref())
        .collect();
    assert!(labels.contains(&"tsouth2@example.com (prolite)"));
    assert!(labels.contains(&"work@example.com (team)"));
}

#[test]
fn provider_cache_still_replaces_the_same_account() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "oauth".to_string(),
    };
    let mut first = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    first.account_id = Some("acct-work".to_string());
    let mut second = first.clone();
    first.error = Some("old".to_string());
    second.error = Some("new".to_string());

    let mut cache = vec![first];
    super::upsert_provider_cache(&mut cache, second);

    // A refresh of the same account updates in place; it does not accumulate.
    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].error.as_deref(), Some("new"));
}

/// The reported failure (#155): first refresh follows the CLI (`account_id =
/// None`). Opening Accounts registers the ambient directory as a real entry,
/// so the next refresh stamps a UUID. Without pruning, both rows stay in the
/// cache and the overview shows two identical Codex cards with no way to
/// remove the ghost (Accounts only lists the registered entry).
#[test]
fn pruning_drops_ambient_ghost_after_directory_account_is_registered() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(0.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "oauth".to_string(),
    };
    let mut ambient =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    ambient.account_id = None;
    ambient.account_email = Some("insan@example.com".into());

    let mut registered = ambient.clone();
    registered.account_id = Some("11111111-2222-3333-4444-555555555555".into());
    registered.account_label = Some("prolite".into());

    let mut cache = vec![ambient, registered.clone()];
    let expected: std::collections::HashSet<_> = [(
        "codex".to_string(),
        Some("11111111-2222-3333-4444-555555555555".to_string()),
    )]
    .into_iter()
    .collect();

    super::prune_stale_provider_readings(&mut cache, &expected, &[ProviderId::Codex]);

    assert_eq!(cache.len(), 1, "ambient ghost must be dropped");
    assert_eq!(
        cache[0].account_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
}

#[test]
fn pruning_keeps_every_configured_account() {
    let personal = account_snapshot("acct-personal", 12.0);
    let work = account_snapshot("acct-work", 88.0);
    let mut cache = vec![personal, work];
    let expected: std::collections::HashSet<_> = [
        ("codex".to_string(), Some("acct-personal".to_string())),
        ("codex".to_string(), Some("acct-work".to_string())),
    ]
    .into_iter()
    .collect();

    super::prune_stale_provider_readings(&mut cache, &expected, &[ProviderId::Codex]);

    assert_eq!(cache.len(), 2);
}

#[test]
fn pruning_drops_disabled_providers() {
    let codex = account_snapshot("acct-codex", 10.0);
    let mut claude = account_snapshot("acct-claude", 20.0);
    claude.provider_id = "claude".to_string();
    let mut cache = vec![codex, claude];
    let expected: std::collections::HashSet<_> =
        [("codex".to_string(), Some("acct-codex".to_string()))]
            .into_iter()
            .collect();

    super::prune_stale_provider_readings(&mut cache, &expected, &[ProviderId::Codex]);

    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].provider_id, "codex");
}

#[test]
fn expected_keys_are_ambient_when_nothing_is_configured() {
    let accounts = ConfiguredAccounts::default();
    let keys = super::expected_cache_keys(&[ProviderId::Codex], &accounts);

    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&("codex".to_string(), None)));
}

#[test]
fn expected_keys_list_every_configured_directory_account() {
    let mut accounts = ConfiguredAccounts::default();
    accounts
        .codex
        .add_account(codexbar::core::DirectoryAccount::<
            codexbar::core::CodexIdentity,
        >::new(
            Some("personal".into()),
            std::path::PathBuf::from("/homes/personal"),
        ));
    accounts
        .codex
        .add_account(codexbar::core::DirectoryAccount::<
            codexbar::core::CodexIdentity,
        >::new(
            Some("work".into()),
            std::path::PathBuf::from("/homes/work"),
        ));

    let keys = super::expected_cache_keys(&[ProviderId::Codex], &accounts);
    let targets = accounts.targets_for(ProviderId::Codex);

    assert_eq!(keys.len(), 2);
    for target in targets {
        assert!(
            keys.contains(&("codex".to_string(), Some(target.id.clone()))),
            "missing target {}",
            target.id
        );
    }
    // Ambient None must not coexist with configured ids.
    assert!(!keys.contains(&("codex".to_string(), None)));
}

#[test]
fn hiding_codex_spark_rows_preserves_other_extra_usage() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let mut snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    snapshot.extra_rate_windows = vec![
        NamedRateWindowSnapshot {
            id: "codex-spark".to_string(),
            title: "Codex Spark 5-hour".to_string(),
            window: snapshot.primary.clone(),
            amount: None,
        },
        NamedRateWindowSnapshot {
            id: "credits".to_string(),
            title: "Credits".to_string(),
            window: snapshot.primary.clone(),
            amount: None,
        },
    ];

    super::filter_hidden_codex_spark_rows(&mut snapshot, false);

    assert_eq!(snapshot.extra_rate_windows.len(), 1);
    assert_eq!(snapshot.extra_rate_windows[0].id, "credits");
}

fn claude_account_snapshot(account_id: &str, used: f64) -> ProviderUsageSnapshot {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(used)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let mut snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    snapshot.account_id = Some(account_id.to_string());
    snapshot
}

fn claude_account_error(account_id: &str) -> ProviderUsageSnapshot {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let mut snapshot = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Unauthorized".to_string(),
    );
    snapshot.account_id = Some(account_id.to_string());
    snapshot
}

#[test]
fn a_transient_failure_never_substitutes_another_accounts_reading() {
    let mut state = crate::state::AppState::new();
    // Only the personal account has a good reading cached.
    state
        .provider_cache
        .push(claude_account_snapshot("acct-personal", 42.0));

    let preserved = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        claude_account_error("acct-work"),
    );

    // Substituting personal's reading here would report one seat's usage under
    // the other's name. The work account has nothing good to fall back to, so
    // its error must surface.
    assert_eq!(preserved.account_id.as_deref(), Some("acct-work"));
    assert!(preserved.error.is_some());
}

#[test]
fn each_account_gets_its_own_transient_failure_grace() {
    let mut state = crate::state::AppState::new();
    state
        .provider_cache
        .push(claude_account_snapshot("acct-personal", 42.0));
    state
        .provider_cache
        .push(claude_account_snapshot("acct-work", 77.0));

    let personal = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        claude_account_error("acct-personal"),
    );
    let work = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        claude_account_error("acct-work"),
    );

    // A shared counter let the first account's failure consume the second's one
    // allowed retry, so the second showed an error it should have ridden out.
    assert_eq!(personal.error, None, "personal lost its grace");
    assert_eq!(
        work.error, None,
        "work lost its grace to personal's failure"
    );
    assert_eq!(
        work.primary.used_percent, 77.0,
        "work got personal's numbers"
    );
}

#[test]
fn claude_transient_auth_failure_preserves_first_last_good_snapshot() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let error = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Unauthorized".to_string(),
    );
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good.clone());

    let preserved = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        error,
    );

    assert_eq!(preserved.error, None);
    assert_eq!(preserved.primary.used_percent, 42.0);
}

#[test]
fn claude_repeated_auth_failure_surfaces_error() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let first_error = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Unauthorized".to_string(),
    );
    let second_error = first_error.clone();
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good);

    let _ = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        first_error,
    );
    let surfaced = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        second_error,
    );

    assert!(surfaced.error.is_some());
}

#[test]
fn claude_error_message_removes_upstream_swift_cancellation() {
    let message = super::friendly_provider_error(
        ProviderId::Claude,
        "The operation couldn't be completed. (Swift.CancellationError error 1.)",
    );

    assert!(!message.contains("Swift"));
    assert!(message.contains("Claude usage fetch was cancelled"));
    assert!(message.contains("Refresh Claude"));
}

#[test]
fn claude_error_message_explains_missing_sign_in() {
    let message = super::friendly_provider_error(
        ProviderId::Claude,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.",
    );

    assert_eq!(
        message,
        "Claude CLI sign-in was not found. Run `claude` once in a terminal, then refresh Claude in Ceiling. Charts can still show local session spend without live capacity."
    );
}

#[test]
fn claude_multi_source_failure_keeps_all_sources_not_oauth_only() {
    let message = super::friendly_provider_error(
        ProviderId::Claude,
        "Claude usage failed from all configured sources. OAuth: OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.; Web: No cookies available for web API; CLI: Provider not installed: claude not found",
    );

    assert!(
        message.contains("Claude capacity could not be loaded"),
        "expected capacity framing, got: {message}"
    );
    assert!(
        message.contains("OAuth: sign-in not found"),
        "expected shortened OAuth detail, got: {message}"
    );
    assert!(
        message.contains("Web: no session available"),
        "expected shortened Web detail, got: {message}"
    );
    assert!(
        message.contains("CLI: not installed:"),
        "expected shortened CLI detail, got: {message}"
    );
    assert!(
        message.contains("Run `claude` once in a terminal"),
        "expected CLI fix hint, got: {message}"
    );
    assert!(
        message.contains("Charts can still show local session spend without live capacity."),
        "expected charts note, got: {message}"
    );
    assert!(
        !message.starts_with("Claude CLI sign-in was not found."),
        "multi-source must not collapse to the OAuth-only line: {message}"
    );
}

#[test]
fn claude_auth_required_does_not_suggest_cookies() {
    let message = super::friendly_provider_error(ProviderId::Claude, "Authentication required");

    assert!(message.contains("CLI sign-in") || message.contains("Run `claude`"));
    assert!(
        !message.to_lowercase().contains("cookie"),
        "must not recommend browser cookies: {message}"
    );
    assert!(message.contains("Charts can still show local session spend without live capacity."));
}

#[test]
fn non_claude_error_message_is_preserved() {
    let message = super::friendly_provider_error(
        ProviderId::Codex,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.",
    );

    assert_eq!(
        message,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate."
    );
}

#[test]
fn chart_data_serde_roundtrip_preserves_fields() {
    use super::{
        DailyCostPoint, DailyUsageBreakdown, LocalUsageScope, ProviderChartData, ServiceUsagePoint,
    };

    let original = ProviderChartData {
        provider_id: "codex".into(),
        cost_history: vec![
            DailyCostPoint {
                date: "2025-01-01".into(),
                value: 1.25,
            },
            DailyCostPoint {
                date: "2025-01-02".into(),
                value: 0.0,
            },
        ],
        credits_history: vec![DailyCostPoint {
            date: "2025-01-01".into(),
            value: 42.0,
        }],
        usage_breakdown: vec![DailyUsageBreakdown {
            day: "2025-01-01".into(),
            services: vec![
                ServiceUsagePoint {
                    service: "gpt-4o".into(),
                    credits_used: 10.0,
                },
                ServiceUsagePoint {
                    service: "gpt-4o-mini".into(),
                    credits_used: 3.5,
                },
            ],
            total_credits_used: 13.5,
        }],
        local_usage: None,
        local_usage_scope: LocalUsageScope::Account,
        quota_history: Vec::new(),
    };

    let json = serde_json::to_string(&original).expect("serialize");
    assert!(
        json.contains("\"providerId\":\"codex\""),
        "camelCase providerId: {json}"
    );
    assert!(json.contains("\"costHistory\""));
    assert!(json.contains("\"creditsHistory\""));
    assert!(json.contains("\"usageBreakdown\""));
    assert!(json.contains("\"localUsage\":null"));
    assert!(json.contains("\"localUsageScope\":\"account\""));
    assert!(json.contains("\"quotaHistory\":[]"));
    assert!(json.contains("\"creditsUsed\":10.0"));
    assert!(json.contains("\"totalCreditsUsed\":13.5"));

    let back: ProviderChartData = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.provider_id, "codex");
    assert_eq!(back.cost_history.len(), 2);
    assert_eq!(back.cost_history[0].date, "2025-01-01");
    assert_eq!(back.credits_history[0].value, 42.0);
    assert_eq!(back.usage_breakdown[0].services.len(), 2);
    assert_eq!(back.usage_breakdown[0].total_credits_used, 13.5);
    assert_eq!(back.local_usage_scope, LocalUsageScope::Account);
}

#[test]
fn chart_data_for_unknown_provider_is_empty() {
    let data =
        super::build_provider_chart_data("this-provider-definitely-does-not-exist".into(), None);
    assert_eq!(data.provider_id, "this-provider-definitely-does-not-exist");
    assert!(data.credits_history.is_empty());
    assert!(data.usage_breakdown.is_empty());
}

#[test]
fn japanese_provider_snapshot_localizes_weekly_label() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_secondary(codexbar::core::RateWindow::new(20.0));
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };

    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

    // Secondary label stays raw; localization happens at render time.
    assert_eq!(snapshot.secondary_label, Some("Weekly".to_string()));
}

#[test]
fn japanese_provider_snapshot_localizes_pace_reserve_description() {
    use chrono::{Duration, Utc};

    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let now = Utc::now();
    // 7-day window, half elapsed, 40% used → 10% ahead of pace, will last to reset.
    let secondary = codexbar::core::RateWindow::with_details(
        40.0,
        Some(7 * 24 * 60),
        Some(now + Duration::minutes(7 * 24 * 60 / 2)),
        None,
    );
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_secondary(secondary);
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };

    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

    // Reserve data stays raw; localization happens at render time.
    let secondary = snapshot.secondary.as_ref().expect("secondary window");
    assert!(secondary.reserve_percent.is_some());
    assert!(secondary.reserve_will_last_to_reset);
    assert!(secondary.reserve_description.is_none());
}

#[test]
fn chart_data_requires_account_email_for_codex() {
    let data = super::build_provider_chart_data("codex".into(), None);
    assert_eq!(data.provider_id, "codex");
    assert!(data.credits_history.is_empty());
    assert!(data.usage_breakdown.is_empty());
}

#[test]
fn region_options_for_regional_provider() {
    let opts = super::region_options_for("alibaba");
    let values: Vec<_> = opts.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values, vec!["singapore", "us", "germany", "hongkong", "cn"]);
}

#[test]
fn minimax_region_options_match_upstream_hosts() {
    let opts = super::region_options_for("minimax");
    let values: Vec<_> = opts.iter().map(|o| o.value.as_str()).collect();
    let labels: Vec<_> = opts.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(values, vec!["global", "cn"]);
    assert_eq!(
        labels,
        vec![
            "Global (platform.minimax.io)",
            "China mainland (platform.minimaxi.com)"
        ]
    );
}

#[test]
fn region_options_empty_for_non_regional_provider() {
    assert!(super::region_options_for("claude").is_empty());
    assert!(super::region_options_for("codex").is_empty());
}

#[test]
fn region_option_roundtrips_serde() {
    let opt = super::RegionOption {
        value: "intl".to_string(),
        label: "International".to_string(),
    };
    let json = serde_json::to_string(&opt).unwrap();
    let back: super::RegionOption = serde_json::from_str(&json).unwrap();
    assert_eq!(opt, back);
}

// ── Phase 6d — credential detection UIs ────────────────────────

#[test]
fn open_path_rejects_empty_path() {
    let err = super::open_path(String::new()).unwrap_err();
    assert!(err.to_lowercase().contains("empty"));
}

#[test]
fn open_path_rejects_relative_path() {
    let err = super::open_path("relative/path".into()).unwrap_err();
    assert!(err.contains("absolute"));
}

#[test]
fn open_path_rejects_missing_path() {
    let missing = std::env::temp_dir()
        .join(format!("codexbar-phase6d-missing-{}", std::process::id()))
        .join("does-not-exist");
    let err = super::open_path(missing.to_string_lossy().into_owned()).unwrap_err();
    assert!(err.contains("not found"));
}

#[cfg(target_os = "windows")]
#[test]
fn windows_shell_path_removes_extended_length_prefixes() {
    assert_eq!(
        super::windows_shell_path(std::path::Path::new(r"\\?\C:\Users\Ceiling")),
        std::path::PathBuf::from(r"C:\Users\Ceiling")
    );
    assert_eq!(
        super::windows_shell_path(std::path::Path::new(r"\\?\UNC\server\share\Ceiling")),
        std::path::PathBuf::from(r"\\server\share\Ceiling")
    );
    assert_eq!(
        super::windows_shell_path(std::path::Path::new(r"C:\Users\Ceiling")),
        std::path::PathBuf::from(r"C:\Users\Ceiling")
    );
}

#[test]
fn open_path_allowlist_accepts_roots_and_exact_paths_only() {
    let root = std::path::PathBuf::from(r"C:\Users\example\AppData\Roaming\Ceiling");
    let exact = std::path::PathBuf::from(r"C:\Program Files\Kiro\kiro-cli.exe");
    assert!(super::system::path_is_allowed(
        &root.join("settings.json"),
        std::slice::from_ref(&root),
        std::slice::from_ref(&exact),
    ));
    assert!(super::system::path_is_allowed(
        &exact,
        std::slice::from_ref(&root),
        std::slice::from_ref(&exact),
    ));
    assert!(!super::system::path_is_allowed(
        std::path::Path::new(r"C:\Windows\System32"),
        &[root],
        &[exact],
    ));
}

#[cfg(target_os = "windows")]
#[test]
fn open_path_rejects_windows_directory() {
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let error = super::open_path(windows).unwrap_err();
    assert!(error.contains("outside Ceiling's allowed locations"));
}

#[test]
fn auxiliary_window_permissions_exclude_sensitive_commands() {
    let permissions = include_str!("../../permissions/commands.toml");
    let floatbar = permissions
        .split("[[permission]]")
        .find(|section| section.contains("identifier = \"floatbar-commands\""))
        .expect("floatbar command permission");
    let flyout = permissions
        .split("[[permission]]")
        .find(|section| section.contains("identifier = \"flyout-commands\""))
        .expect("flyout command permission");

    for command in [
        "set_api_key",
        "set_manual_cookie",
        "add_token_account",
        "open_path",
        "quit_app",
        "run_proof_command",
        "apply_update",
    ] {
        assert!(!floatbar.contains(&format!("\"{command}\"")));
        assert!(!flyout.contains(&format!("\"{command}\"")));
    }
}

#[test]
fn external_url_validator_accepts_http_and_https() {
    assert_eq!(
        super::validate_external_url(" https://github.com/Finesssee/Win-CodexBar "),
        Ok("https://github.com/Finesssee/Win-CodexBar")
    );
    assert_eq!(
        super::validate_external_url("http://localhost:1420"),
        Ok("http://localhost:1420")
    );
}

#[test]
fn external_url_validator_rejects_non_web_and_control_urls() {
    assert!(super::validate_external_url("file:///C:/Windows/win.ini").is_err());
    assert!(super::validate_external_url("javascript:alert(1)").is_err());
    assert!(super::validate_external_url("https://example.com/\nmalicious").is_err());
}

// ── Phase 13 — E2E IPC harness ─────────────────────────────────
//
// Build the full bootstrap payload and prove that every shared
// `ProviderId` variant ends up in the provider catalog with a
// non-empty id + display name. If a new provider is added to the
// enum but never wired through the desktop catalog, this test will
// fail with `missing provider in bootstrap catalog: <id>`.

#[test]
fn bootstrap_payload_exposes_every_provider_variant() {
    let payload = super::get_bootstrap_state();

    let catalog_ids: std::collections::HashSet<String> = payload
        .providers
        .iter()
        .map(|entry| entry.id.clone())
        .collect();

    for entry in &payload.providers {
        assert!(!entry.id.is_empty(), "provider entry has empty id");
        assert!(
            !entry.display_name.is_empty(),
            "provider {} has empty display_name",
            entry.id
        );
    }

    for provider in ProviderId::all() {
        let expected = provider.cli_name().to_string();
        assert!(
            catalog_ids.contains(&expected),
            "missing provider in bootstrap catalog: {expected}"
        );
    }

    assert_eq!(
        catalog_ids.len(),
        ProviderId::all().len(),
        "bootstrap catalog size drifted from ProviderId::all()"
    );

    // Sanity — payload must also round-trip through JSON cleanly so
    // the TypeScript bridge never sees a partially-populated record.
    let encoded = serde_json::to_string(&payload).expect("serialize bootstrap");
    assert!(encoded.contains("contractVersion"));
    assert!(encoded.contains("\"providers\""));
    assert!(encoded.contains("\"settings\""));
}
