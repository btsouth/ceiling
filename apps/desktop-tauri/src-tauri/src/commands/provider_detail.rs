use super::*;

// ── Provider detail pane (Phase 6b) ──────────────────────────────────

/// One selectable account on the provider detail pane.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDetailAccount {
    pub account_id: String,
    pub label: String,
    pub tint: Option<String>,
}

/// DTO for the provider detail pane in the Settings Providers tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDetail {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,

    // Identity
    /// Account this payload describes. `None` when the provider has no
    /// configured accounts and the reading is whatever the CLI is signed in as.
    pub account_id: Option<String>,
    /// Every account with a reading for this provider, so the pane can offer a
    /// selector. One entry means there is nothing to choose between.
    pub accounts: Vec<ProviderDetailAccount>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub auth_type: Option<String>,
    pub source_label: Option<String>,
    pub organization: Option<String>,
    pub last_updated: Option<String>,

    // Usage windows — reuse existing RateWindowSnapshot shape.
    pub session: Option<RateWindowSnapshot>,
    pub weekly: Option<RateWindowSnapshot>,
    pub model_specific: Option<RateWindowSnapshot>,
    pub tertiary: Option<RateWindowSnapshot>,
    pub extra_rate_windows: Vec<NamedRateWindowSnapshot>,

    // Cost / pace.
    pub cost: Option<CostSnapshotBridge>,
    pub pace: Option<PaceSnapshot>,

    // Error / state.
    pub last_error: Option<String>,

    // URLs for quick-actions (button visibility).
    pub dashboard_url: Option<String>,
    pub status_page_url: Option<String>,
    pub buy_credits_url: Option<String>,
    /// True when `trigger_provider_login` can sign this provider in from the
    /// app (gates the "Switch account" quick action).
    pub supports_login: bool,

    // True if the shared backend has produced any snapshot yet.
    pub has_snapshot: bool,

    // Phase 6c — currently-persisted cookie source & region for round-tripping
    // into the settings UI pickers. `None` for providers that do not support
    // one of the pickers.
    pub cookie_source: Option<String>,
    pub region: Option<String>,
}

pub(crate) fn build_provider_detail(provider_id: &str) -> Result<ProviderDetail, String> {
    let id = parse_provider_arg(provider_id)?;

    let settings = Settings::load();
    let enabled = settings
        .enabled_providers
        .iter()
        .any(|p| p == id.cli_name());

    let provider = instantiate_provider(id);
    let metadata = provider.metadata();
    let dashboard_url = if id == codexbar::core::ProviderId::MiniMax {
        Some(
            codexbar::providers::MiniMaxProvider::dashboard_url_for_region(Some(
                settings.api_region(id),
            )),
        )
    } else {
        metadata.dashboard_url.map(|s| s.to_string())
    };

    Ok(ProviderDetail {
        id: id.cli_name().to_string(),
        display_name: id.display_name().to_string(),
        enabled,
        account_id: None,
        accounts: Vec::new(),
        email: None,
        plan: None,
        auth_type: None,
        source_label: None,
        organization: None,
        last_updated: None,
        session: None,
        weekly: None,
        model_specific: None,
        tertiary: None,
        extra_rate_windows: Vec::new(),
        cost: None,
        pace: None,
        last_error: None,
        dashboard_url: dashboard_url.clone(),
        status_page_url: metadata.status_page_url.map(|s| s.to_string()),
        // Buy-credits currently mirrors the dashboard URL for providers that
        // support credit top-ups; refine once a dedicated URL lands upstream.
        buy_credits_url: if metadata.supports_credits {
            dashboard_url
        } else {
            None
        },
        supports_login: super::system::supports_in_app_login(id),
        has_snapshot: false,
        cookie_source: provider_cookie_source_lookup(&settings, id.cli_name()),
        region: provider_region_lookup(&settings, id.cli_name()),
    })
}

#[tauri::command]
pub fn get_provider_detail(
    app: tauri::AppHandle,
    provider_id: String,
    account_id: Option<String>,
) -> Result<ProviderDetail, String> {
    let mut detail = build_provider_detail(&provider_id)?;

    // Merge the latest cached snapshot, if any.
    let state = app.state::<Mutex<AppState>>();
    if let Ok(guard) = state.lock() {
        let readings: Vec<&ProviderUsageSnapshot> = guard
            .provider_cache
            .iter()
            .filter(|s| s.provider_id == detail.id)
            .collect();

        detail.accounts = readings
            .iter()
            .filter_map(|snap| {
                Some(ProviderDetailAccount {
                    account_id: snap.account_id.clone()?,
                    label: snap
                        .account_label
                        .clone()
                        .unwrap_or_else(|| detail.display_name.clone()),
                    tint: snap.account_tint.clone(),
                })
            })
            .collect();

        // An explicit choice wins. Without one, summarise the account closest to
        // its limit rather than whichever reading happened to be first, which is
        // what this did when a provider could only ever have one.
        let chosen = account_id
            .as_deref()
            .and_then(|wanted| {
                readings
                    .iter()
                    .find(|snap| snap.account_id.as_deref() == Some(wanted))
            })
            .or_else(|| {
                readings.iter().max_by(|a, b| {
                    a.primary
                        .used_percent
                        .total_cmp(&b.primary.used_percent)
                        .then_with(|| b.account_id.cmp(&a.account_id))
                })
            })
            .copied();

        if let Some(snap) = chosen {
            let mut snapshot = snap.clone();
            super::filter_hidden_codex_spark_rows(
                &mut snapshot,
                Settings::load().codex_spark_usage_visible(),
            );
            detail.account_id = snapshot.account_id.clone();
            detail.email = snapshot.account_email.clone();
            detail.plan = snapshot.plan_name.clone();
            detail.organization = snapshot.account_organization.clone();
            detail.source_label = if snapshot.source_label.is_empty() {
                None
            } else {
                Some(snapshot.source_label.clone())
            };
            detail.last_updated = Some(snapshot.updated_at.clone());
            if snapshot.error.is_none() {
                detail.session = Some(snapshot.primary.clone());
                detail.weekly = snapshot.secondary.clone();
                detail.model_specific = snapshot.model_specific.clone();
                detail.tertiary = snapshot.tertiary.clone();
                detail.extra_rate_windows = snapshot.extra_rate_windows.clone();
                detail.cost = snapshot.cost.clone();
                detail.pace = snapshot.pace.clone();
            }
            detail.last_error = snapshot.error.clone();
            detail.has_snapshot = true;
        }
    }

    Ok(detail)
}

#[tauri::command]
pub fn revoke_provider_credentials(provider_id: String) -> Result<(), String> {
    // Best-effort: drop every app-managed credential for this provider so the
    // caller can follow up with a fresh login or import. Missing entries are
    // silently ignored; only I/O errors propagate.
    let id = parse_provider_arg(&provider_id)?;
    codexbar::settings::revoke_managed_credentials(id).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStoreStatusBridge {
    /// Protection/readability of the shared file that contains credentials for
    /// every provider. This is deliberately distinct from provider presence.
    pub file_status: String,
    /// Whether the selected provider has an entry in this store. `None` means
    /// the store could not be decoded, so absence cannot be asserted safely.
    pub has_provider_credentials: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStorageStatusBridge {
    pub manual_cookies: CredentialStoreStatusBridge,
    pub api_keys: CredentialStoreStatusBridge,
    pub token_accounts: CredentialStoreStatusBridge,
}

pub(crate) fn credential_file_status_label(status: SecureFileStatus) -> String {
    match status {
        SecureFileStatus::Missing => "missing".to_string(),
        SecureFileStatus::Plaintext => "plaintext".to_string(),
        SecureFileStatus::Protected(protection) => format!("protected:{protection}"),
        SecureFileStatus::Unreadable(_) => "unreadable".to_string(),
    }
}

fn provider_store_status(
    file_status: Option<SecureFileStatus>,
    has_provider_credentials: Option<bool>,
) -> CredentialStoreStatusBridge {
    let has_provider_credentials = match file_status.as_ref() {
        Some(SecureFileStatus::Missing) => Some(false),
        Some(SecureFileStatus::Unreadable(_)) | None => None,
        Some(SecureFileStatus::Plaintext | SecureFileStatus::Protected(_)) => {
            has_provider_credentials
        }
    };
    CredentialStoreStatusBridge {
        file_status: file_status
            .map(credential_file_status_label)
            .unwrap_or_else(|| "unavailable".to_string()),
        has_provider_credentials,
    }
}

pub(crate) struct CredentialStorageSources<'a> {
    pub manual_cookie_file_status: Option<SecureFileStatus>,
    pub manual_cookies: Option<&'a ManualCookies>,
    pub api_key_file_status: Option<SecureFileStatus>,
    pub api_keys: Option<&'a ApiKeys>,
    pub token_account_file_status: SecureFileStatus,
    pub token_accounts: Option<&'a HashMap<ProviderId, ProviderAccountData>>,
}

pub(crate) fn build_credential_storage_status(
    provider: ProviderId,
    sources: CredentialStorageSources<'_>,
) -> CredentialStorageStatusBridge {
    let provider_id = provider.cli_name();
    CredentialStorageStatusBridge {
        manual_cookies: provider_store_status(
            sources.manual_cookie_file_status,
            sources
                .manual_cookies
                .map(|store| store.get(provider_id).is_some()),
        ),
        api_keys: provider_store_status(
            sources.api_key_file_status,
            sources
                .api_keys
                .map(|store| store.get(provider_id).is_some()),
        ),
        token_accounts: provider_store_status(
            Some(sources.token_account_file_status),
            sources.token_accounts.map(|store| {
                store
                    .get(&provider)
                    .is_some_and(|accounts| !accounts.accounts.is_empty())
            }),
        ),
    }
}

#[tauri::command]
pub fn get_credential_storage_status(
    provider_id: String,
) -> Result<CredentialStorageStatusBridge, String> {
    let provider = parse_provider_arg(&provider_id)?;
    let manual_cookie_file_status =
        ManualCookies::cookies_path().map(|path| secure_file::status(&path));
    let api_key_file_status = ApiKeys::keys_path().map(|path| secure_file::status(&path));
    let token_store = TokenAccountStore::new();
    let token_account_file_status = secure_file::status(&TokenAccountStore::default_path());

    let manual_cookies = ManualCookies::try_load_for_read().ok();
    let api_keys = ApiKeys::try_load_for_read().ok();
    let token_accounts = token_store.load().ok();

    Ok(build_credential_storage_status(
        provider,
        CredentialStorageSources {
            manual_cookie_file_status,
            manual_cookies: manual_cookies.as_ref(),
            api_key_file_status,
            api_keys: api_keys.as_ref(),
            token_account_file_status,
            token_accounts: token_accounts.as_ref(),
        },
    ))
}
