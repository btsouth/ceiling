use super::*;
use std::sync::Arc;

const MAX_CONCURRENT_PROVIDER_FETCHES: usize = 8;

// ── Provider refresh commands ────────────────────────────────────────

/// Build a `FetchContext` for a provider using persisted cookies/keys.
pub(crate) fn build_fetch_context(
    id: ProviderId,
    settings: &Settings,
    cookies: &ManualCookies,
    api_keys: &ApiKeys,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> FetchContext {
    let cookie_source = settings.cookie_source(id);
    let stored_cookie = cookies.get(id.cli_name()).map(|s| s.to_string());
    let stored_api_key = api_keys.get(id.cli_name()).map(|s| s.to_string());
    let token_override = token_accounts
        .get(&id)
        .and_then(|data| data.active_account())
        .cloned()
        .map(|account| TokenAccountOverride::from_account(id, account));
    let active_token_cookie = token_override
        .as_ref()
        .and_then(|override_data| override_data.cookie_header.clone());
    let active_token_env = token_override
        .as_ref()
        .and_then(|override_data| override_data.env_override.as_ref());
    let active_token_api_key = active_token_env.and_then(|env| env.values().next().cloned());
    let usage_source = SourceMode::parse(settings.usage_source(id)).unwrap_or_default();
    let api_key = stored_api_key.or(active_token_api_key);
    let has_kimi_code_api_key =
        id == ProviderId::Kimi && api_key.as_deref().is_some_and(|key| !key.trim().is_empty());

    let (source_mode, cookie_header) = if id.cookie_domain().is_none() {
        let source_mode = if active_token_env.is_some() {
            SourceMode::OAuth
        } else {
            usage_source
        };
        (source_mode, None)
    } else {
        match cookie_source {
            _ if active_token_env.is_some() => (SourceMode::OAuth, None),
            "off" if id == ProviderId::Claude && usage_source != SourceMode::Cli => {
                (SourceMode::OAuth, None)
            }
            // Grok uses `~/.grok/auth.json` from `grok login` (and optional browser
            // cookies). Do not force Cli when cookies are disabled/empty.
            "off" if id == ProviderId::Grok && usage_source != SourceMode::Cli => {
                (SourceMode::Auto, None)
            }
            "off" if has_kimi_code_api_key && usage_source == SourceMode::Auto => {
                (SourceMode::Auto, None)
            }
            "off" => (SourceMode::Cli, None),
            "manual" => {
                let cookie_header = active_token_cookie.or(stored_cookie);
                let source_mode = if has_kimi_code_api_key && usage_source == SourceMode::Auto {
                    SourceMode::Auto
                } else if cookie_header.is_some() {
                    SourceMode::Web
                } else if id == ProviderId::Claude && usage_source != SourceMode::Cli {
                    SourceMode::OAuth
                } else if id == ProviderId::Grok {
                    // Local CLI auth file; provider also tries browser cookies.
                    SourceMode::Auto
                } else if id == ProviderId::Cursor {
                    // Provider resolves IDE disk session / browser cookies itself.
                    SourceMode::Web
                } else {
                    SourceMode::Cli
                };
                (source_mode, cookie_header)
            }
            // `browser` is accepted as a legacy alias from older settings.
            "auto" | "browser" | "web" => {
                // Modern browsers deliberately prevent reliable database
                // extraction. Automatic sources may still use CLI/IDE auth,
                // but website cookies must be supplied explicitly.
                let cookie_header = active_token_cookie.or(stored_cookie);
                (usage_source, cookie_header)
            }
            _ => (usage_source, stored_cookie),
        }
    };

    let workspace_id = settings.workspace_id(id).trim().to_string();
    let api_region = settings.api_region(id).trim().to_string();
    let gateway_url = (id == ProviderId::Wayfinder && !settings.gateway_url(id).is_empty())
        .then(|| settings.gateway_url(id).to_string());

    FetchContext {
        source_mode,
        manual_cookie_header: cookie_header,
        api_key,
        workspace_id: (!workspace_id.is_empty()).then_some(workspace_id),
        api_region: (!api_region.is_empty()).then_some(api_region),
        gateway_url,
        ..FetchContext::default()
    }
}

const DEFAULT_PROVIDER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
const SLOW_PROVIDER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);
const MAX_CONTEXT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(65);

pub(crate) fn provider_fetch_timeout(id: ProviderId, ctx: &FetchContext) -> std::time::Duration {
    let provider_timeout = match id {
        ProviderId::Claude | ProviderId::Codex | ProviderId::Copilot | ProviderId::Grok => {
            SLOW_PROVIDER_FETCH_TIMEOUT
        }
        _ => DEFAULT_PROVIDER_FETCH_TIMEOUT,
    };
    let context_timeout = std::time::Duration::from_secs(ctx.web_timeout.saturating_add(5));
    provider_timeout.max(context_timeout.min(MAX_CONTEXT_FETCH_TIMEOUT))
}

pub(crate) fn is_provider_cache_fresh(
    updated_at: Option<std::time::Instant>,
    stale_after: std::time::Duration,
) -> bool {
    updated_at
        .map(|updated| updated.elapsed() <= stale_after)
        .unwrap_or(false)
}

pub(crate) fn upsert_provider_cache(
    cache: &mut Vec<ProviderUsageSnapshot>,
    snapshot: ProviderUsageSnapshot,
) {
    // Keyed by provider *and* account: one provider can now have a reading per
    // configured account, and keying on provider alone made the second account
    // overwrite the first.
    if let Some(existing) = cache.iter_mut().find(|existing| {
        existing.provider_id == snapshot.provider_id && existing.account_id == snapshot.account_id
    }) {
        *existing = snapshot;
    } else {
        cache.push(snapshot);
    }
}

/// One cache row identity: provider CLI name + optional configured-account id.
type ProviderCacheKey = (String, Option<String>);

/// The set of readings a refresh cycle will (or did) produce.
///
/// Empty targets means a single ambient fetch with `account_id = None`.
/// Configured directories each produce one reading stamped with their id.
pub(crate) fn expected_cache_keys(
    enabled_ids: &[ProviderId],
    account_dirs: &ConfiguredAccounts,
) -> HashSet<ProviderCacheKey> {
    let mut keys = HashSet::new();
    for id in enabled_ids {
        let targets = account_dirs.targets_for(*id);
        if targets.is_empty() {
            keys.insert((id.cli_name().to_string(), None));
        } else {
            for target in targets {
                keys.insert((id.cli_name().to_string(), Some(target.id)));
            }
        }
    }
    keys
}

/// Drop cache rows that do not belong to this cycle's expected set.
///
/// Upsert alone cannot remove a row whose key changed: an ambient reading
/// (`account_id = None`) and a later registered-account reading (`Some(uuid)`)
/// are different keys, so both stay unless pruned. Disabled providers are
/// dropped entirely so a disabled seat cannot linger on the overview.
pub(crate) fn prune_stale_provider_readings(
    cache: &mut Vec<ProviderUsageSnapshot>,
    expected: &HashSet<ProviderCacheKey>,
    enabled_ids: &[ProviderId],
) {
    let enabled: HashSet<&str> = enabled_ids.iter().map(|id| id.cli_name()).collect();
    cache.retain(|snap| {
        if !enabled.contains(snap.provider_id.as_str()) {
            return false;
        }
        expected.contains(&(snap.provider_id.clone(), snap.account_id.clone()))
    });
}

/// Core refresh logic, usable from both the Tauri command and tray menu actions.
pub(crate) async fn do_refresh_providers(app: &tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_with_policy(app, true).await
}

pub(crate) async fn do_refresh_providers_if_stale(app: &tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_with_policy(app, false).await
}

async fn do_refresh_providers_with_policy(
    app: &tauri::AppHandle,
    force: bool,
) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();

    if !begin_provider_refresh(&state, force)? {
        return Ok(());
    }

    let inputs = ProviderRefreshInputs::load();
    events::emit_refresh_started(
        app,
        inputs
            .enabled_ids
            .iter()
            .map(|id| id.cli_name().to_string())
            .collect(),
    );
    let enabled_count = inputs.enabled_ids.len();

    let handles = spawn_provider_refreshes(app, &inputs);
    await_provider_refreshes(handles).await;

    // After every fetch has landed, drop readings this cycle did not own.
    // Without this, ambient (`account_id = None`) rows survive next to the
    // registered UUID reading once Accounts auto-registers the signed-in
    // directory — two identical cards for one account (issue #155).
    let expected = expected_cache_keys(&inputs.enabled_ids, &inputs.account_dirs);
    if let Ok(mut guard) = state.lock() {
        prune_stale_provider_readings(&mut guard.provider_cache, &expected, &inputs.enabled_ids);
    }

    let error_count = finish_provider_refresh(&state)?;
    update_tray_and_notifications(app, &state, &inputs.settings, &inputs.token_accounts)?;
    persist_widget_snapshot_from_cache(&state, &inputs.enabled_ids);
    if let Ok(mut guard) = state.lock() {
        guard.notification_manager.arm_after_startup_baseline();
    }

    events::emit_refresh_complete(app, enabled_count, error_count);
    crate::auto_refresh::schedule_refresh_enrichment(app, &inputs.settings);

    Ok(())
}

fn begin_provider_refresh(
    state: &tauri::State<'_, Mutex<AppState>>,
    force: bool,
) -> Result<bool, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.is_refreshing {
        return Ok(false);
    }
    if provider_cache_can_skip_refresh(&guard, force) {
        return Ok(false);
    }

    guard.is_refreshing = true;
    guard.provider_refresh_started_at = Some(std::time::Instant::now());
    guard.notification_manager.begin_refresh_cycle();
    Ok(true)
}

fn provider_cache_can_skip_refresh(guard: &AppState, force: bool) -> bool {
    !force
        && !guard.provider_cache.is_empty()
        && is_provider_cache_fresh(guard.provider_cache_updated_at, PROVIDER_CACHE_STALE_AFTER)
}

struct ProviderRefreshInputs {
    settings: Settings,
    enabled_ids: Vec<ProviderId>,
    manual_cookies: ManualCookies,
    api_keys: ApiKeys,
    token_accounts: HashMap<ProviderId, ProviderAccountData>,
    account_dirs: ConfiguredAccounts,
}

impl ProviderRefreshInputs {
    fn load() -> Self {
        let settings = Settings::load();
        let enabled_ids = settings.get_enabled_provider_ids();
        let manual_cookies = ManualCookies::load();
        let api_keys = ApiKeys::load();
        let token_accounts = TokenAccountStore::new().load().unwrap_or_else(|e| {
            tracing::warn!("failed to load token accounts for provider refresh: {e}");
            HashMap::new()
        });

        Self {
            settings,
            enabled_ids,
            manual_cookies,
            api_keys,
            token_accounts,
            account_dirs: ConfiguredAccounts::load(),
        }
    }
}

/// Note which account each config directory holds right now.
///
/// Runs on the refresh cycle because that is already when Ceiling reads these
/// credentials. It is what lets local activity be attributed to an account in
/// the common setup where someone keeps one config directory and re-runs
/// `codex login` to switch, since the logs themselves carry no account id.
fn record_account_observations(accounts: &ConfiguredAccounts) {
    codexbar::core::AccountLedger::record_and_persist(accounts, chrono::Utc::now().timestamp());
}

fn spawn_provider_refreshes(
    app: &tauri::AppHandle,
    inputs: &ProviderRefreshInputs,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(inputs.enabled_ids.len());
    let fetch_permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROVIDER_FETCHES));

    record_account_observations(&inputs.account_dirs);

    for id in &inputs.enabled_ids {
        let id = *id;
        let base_ctx = build_fetch_context(
            id,
            &inputs.settings,
            &inputs.manual_cookies,
            &inputs.api_keys,
            &inputs.token_accounts,
        );

        // One fetch per configured account, so several accounts on one provider
        // are read side by side rather than replacing each other. With none
        // configured this is a single ambient fetch, exactly as before.
        let fetches = account_fetches(id, base_ctx, &inputs.account_dirs);

        for (ctx, account) in fetches {
            let app_handle = app.clone();
            let fetch_permits = Arc::clone(&fetch_permits);
            handles.push(tokio::spawn(async move {
                let Ok(_permit) = fetch_permits.acquire_owned().await else {
                    return;
                };
                refresh_provider(app_handle, id, ctx, account).await;
            }));
        }
    }

    handles
}

/// One fetch context per configured account, or a single ambient fetch.
///
/// Claude directory accounts drop the global session cookie/key here so Web
/// mode cannot paint every card from one `sessionKey` (SBS-1034 / SBS-695).
pub(crate) fn account_fetches(
    id: ProviderId,
    base_ctx: FetchContext,
    account_dirs: &ConfiguredAccounts,
) -> Vec<(FetchContext, AccountBadge)> {
    let targets = account_dirs.targets_for(id);
    if targets.is_empty() {
        return vec![(base_ctx, AccountBadge::default())];
    }
    targets
        .into_iter()
        .map(|target| {
            // Carried alongside the reading so every surface can say
            // which account it shows, rather than re-reading the store.
            let badge = AccountBadge {
                id: Some(target.id),
                label: Some(target.label),
                tint: target.tint,
            };
            let ctx = base_ctx
                .clone()
                .pinned_to_account_dir(id, target.config_dir);
            (ctx, badge)
        })
        .collect()
}

/// The active account's display identity, attached to each reading.
#[derive(Debug, Clone, Default)]
pub(crate) struct AccountBadge {
    /// Stable account id; `None` while following the CLI.
    pub id: Option<String>,
    pub label: Option<String>,
    pub tint: Option<String>,
}

async fn refresh_provider(
    app: tauri::AppHandle,
    id: ProviderId,
    ctx: FetchContext,
    account: AccountBadge,
) {
    let mut snapshot = fetch_provider_snapshot(id, ctx).await;
    // Set on failures too: a card that cannot fetch should still say which
    // account it was trying to read.
    snapshot.account_id = account.id;
    snapshot.account_label = account.label;
    snapshot.account_tint = account.tint;

    let state = app.state::<Mutex<AppState>>();
    let (snapshot, capacity_events, notifications_armed) = if let Ok(mut guard) = state.lock() {
        let mut snapshot = preserve_last_good_transient_failure(&mut guard, id, snapshot);
        // Detect resets/lifts against the real response first, so the synthetic
        // `unavailable` rows added next are never mistaken for a lifted window.
        let capacity_events = guard.capacity_event_observer.observe(&snapshot);
        let unavailable = guard.enforcement_tracker.annotate(&mut snapshot);
        if !unavailable.is_empty() {
            tracing::debug!(
                provider = %snapshot.provider_id,
                windows = ?unavailable,
                "windows dropped out of a successful response; marked unavailable"
            );
        }
        let notifications_armed = guard.notification_manager.notifications_are_armed();
        upsert_provider_cache(&mut guard.provider_cache, snapshot.clone());
        (snapshot, capacity_events, notifications_armed)
    } else {
        (snapshot, Vec::new(), false)
    };
    crate::usage_history::record_snapshot(&snapshot);
    events::emit_provider_updated(&app, &snapshot);
    let notification_settings = Settings::load();

    // The startup gate exists so re-establishing state at launch does not toast.
    // Away events are the exception: they are detected on exactly that first
    // cycle and are the only chance to report a reset that happened while
    // Ceiling was closed, so suppressing them means never mentioning it at all.
    let capacity_events: Vec<_> = if notifications_armed {
        capacity_events
    } else {
        let (away, suppressed): (Vec<_>, Vec<_>) = capacity_events
            .into_iter()
            .partition(|event| event.while_away);
        for event in &suppressed {
            tracing::debug!(
                provider = %event.provider_id,
                kind = ?event.kind,
                "suppressing capacity event while establishing startup baseline"
            );
        }
        away
    };
    // Persist completed runs for every classified reset we keep, including away
    // events that skip the toast gate. Do this before the empty early-return so
    // a quiet launch that only reports while_away still records history.
    if !capacity_events.is_empty() {
        crate::quota_run_history::record_capacity_events(&capacity_events, &snapshot);
    }
    // Apply classified resets before the snapshot-driven fallback. Otherwise
    // the observed drop opens a new run that the classified event immediately
    // closes again as a zero-duration duplicate.
    crate::quota_run_history::record_snapshot(&snapshot);
    if capacity_events.is_empty() {
        return;
    }

    for event in &capacity_events {
        events::emit_capacity_event(&app, event);
    }
    if notification_settings.show_notifications
        && notification_settings.capacity_event_notifications_enabled
        && let Some((title, body)) = capacity_event_notification(&capacity_events)
    {
        let while_away = capacity_events.iter().any(|event| event.while_away);
        let notified = match state.lock() {
            Ok(mut guard) if while_away => guard
                .notification_manager
                .notify_capacity_event_while_away(&title, &body),
            Ok(mut guard) => guard
                .notification_manager
                .notify_capacity_event(&title, &body),
            Err(error) => {
                tracing::warn!("failed to lock app state for capacity notification: {error}");
                false
            }
        };
        if notified {
            codexbar::sound::play_alert(
                codexbar::sound::AlertSound::Success,
                &notification_settings,
            );
        }
    }
}

fn capacity_event_uses_windows_notification(
    event: &crate::capacity_events::CapacityEventPayload,
) -> bool {
    use crate::capacity_events::CapacityEventKind;
    match event.kind {
        // A 5-hour session rolling over on schedule happens several times a day
        // and is precisely what the user already expects. Toasting it trains
        // people to swipe Ceiling away, and it crowded out the weekly and
        // monthly boundaries that actually change what someone can do today.
        CapacityEventKind::ScheduledReset => event.is_long_window(),
        // Capacity arriving early or out of band is news at any cadence.
        CapacityEventKind::SurpriseReset
        | CapacityEventKind::PartialReset
        | CapacityEventKind::BankedResetGranted => true,
        CapacityEventKind::ResetTimeShift
        | CapacityEventKind::WindowLifted
        | CapacityEventKind::WindowRestored
        | CapacityEventKind::AllowanceGranted => false,
    }
}

fn capacity_event_notification(
    events: &[crate::capacity_events::CapacityEventPayload],
) -> Option<(String, String)> {
    let eligible = events
        .iter()
        .filter(|event| capacity_event_uses_windows_notification(event))
        .collect::<Vec<_>>();
    match eligible.as_slice() {
        [] => None,
        [event] => Some((event.notification_title(), event.notification_body())),
        events => {
            let provider = &events[0].display_name;
            let body = events
                .iter()
                .map(|event| {
                    if event.kind == crate::capacity_events::CapacityEventKind::BankedResetGranted {
                        let available = event.current_reset_credits.unwrap_or(1);
                        format!(
                            "{available} banked reset{} available",
                            if available == 1 { "" } else { "s" }
                        )
                    } else {
                        format!(
                            "{} {:.0}% → {:.0}% used",
                            event.window_label,
                            event.previous_used_percent,
                            event.current_used_percent
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            Some((format!("{provider} capacity restored"), format!("{body}.")))
        }
    }
}

pub(super) fn preserve_last_good_transient_failure(
    guard: &mut AppState,
    id: ProviderId,
    snapshot: ProviderUsageSnapshot,
) -> ProviderUsageSnapshot {
    // Scoped to the account, not just the provider. Substituting another
    // account's reading here would report one seat's usage under the other's
    // name, and a shared counter let one account's failure consume the other's
    // single allowed retry.
    let key = (id, snapshot.account_id.clone());

    if snapshot.error.is_none() {
        guard.transient_provider_failure_counts.remove(&key);
        return snapshot;
    }

    if id != ProviderId::Claude || !is_transient_claude_auth_error(snapshot.error.as_deref()) {
        guard.transient_provider_failure_counts.remove(&key);
        return snapshot;
    }

    let Some(previous) = guard
        .provider_cache
        .iter()
        .find(|cached| {
            cached.provider_id == id.cli_name()
                && cached.account_id == snapshot.account_id
                && cached.error.is_none()
        })
        .cloned()
    else {
        return snapshot;
    };

    let count = guard
        .transient_provider_failure_counts
        .entry(key)
        .or_insert(0);
    if *count == 0 {
        *count = 1;
        tracing::warn!(
            provider = id.cli_name(),
            "preserving last good provider snapshot after transient auth failure"
        );
        previous
    } else {
        *count = count.saturating_add(1);
        snapshot
    }
}

fn is_transient_claude_auth_error(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let lower = error.to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("authentication required")
        || lower.contains("auth required")
        || lower.contains("oauth")
}

async fn fetch_provider_snapshot(id: ProviderId, ctx: FetchContext) -> ProviderUsageSnapshot {
    let provider = instantiate_provider(id);
    let metadata = provider.metadata().clone();
    let started = std::time::Instant::now();

    let mut snapshot =
        match tokio::time::timeout(provider_fetch_timeout(id, &ctx), provider.fetch_usage(&ctx))
            .await
        {
            Ok(Ok(result)) => ProviderUsageSnapshot::from_fetch_result(id, &metadata, &result),
            Ok(Err(e)) => ProviderUsageSnapshot::from_error(
                id,
                &metadata,
                codexbar::logging::safe_error_message(e),
            ),
            Err(_) => ProviderUsageSnapshot::from_error(id, &metadata, "Timeout".to_string()),
        };

    record_provider_fetch_duration(id, &mut snapshot, started);
    snapshot
}

fn record_provider_fetch_duration(
    id: ProviderId,
    snapshot: &mut ProviderUsageSnapshot,
    started: std::time::Instant,
) {
    let fetch_duration_ms = started.elapsed().as_millis();
    snapshot.fetch_duration_ms = Some(fetch_duration_ms);
    if fetch_duration_ms > 5_000 {
        tracing::warn!(
            provider = id.cli_name(),
            fetch_duration_ms,
            "slow provider refresh"
        );
    }
}

async fn await_provider_refreshes(handles: Vec<tokio::task::JoinHandle<()>>) {
    for handle in handles {
        let _ = handle.await;
    }
}

fn finish_provider_refresh(state: &tauri::State<'_, Mutex<AppState>>) -> Result<usize, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.is_refreshing = false;
    guard.provider_cache_updated_at = Some(std::time::Instant::now());
    guard.provider_refresh_started_at = None;
    Ok(guard
        .provider_cache
        .iter()
        .filter(|s| s.error.is_some())
        .count())
}

/// Persist the in-memory provider cache as `widget-snapshot.json` so
/// `codexbar statusline` / `codexbar mcp` can read quota without networking.
/// One reading per provider for the always-visible taskbar strip, choosing the
/// account closest to its limit.
///
/// That strip has room for one entry per provider and exists to warn you, so
/// the seat about to run out is the one worth surfacing. This is the same
/// principle already applied to windows: show what is actually constraining
/// you, not whichever reading arrived last. Hovering opens the flyout, which
/// shows every account with its own label.
///
/// Without this a provider with two accounts emitted two entries under one
/// `ProviderId`, which is ambiguous for every consumer downstream.
pub(super) fn most_constrained_per_provider(
    cached: &[ProviderUsageSnapshot],
) -> Vec<&ProviderUsageSnapshot> {
    let mut best: Vec<&ProviderUsageSnapshot> = Vec::new();
    for snapshot in cached.iter().filter(|snap| snap.error.is_none()) {
        match best
            .iter_mut()
            .find(|existing| existing.provider_id == snapshot.provider_id)
        {
            Some(existing) => {
                let existing_used = existing.primary.used_percent;
                let used = snapshot.primary.used_percent;
                // Ties resolve on account id so the strip does not flicker
                // between accounts as readings land in different orders.
                let replace = used > existing_used
                    || (used == existing_used && snapshot.account_id < existing.account_id);
                if replace {
                    *existing = snapshot;
                }
            }
            None => best.push(snapshot),
        }
    }
    best
}

fn persist_widget_snapshot_from_cache(
    state: &tauri::State<'_, Mutex<AppState>>,
    enabled_ids: &[ProviderId],
) {
    let cached = match state.lock() {
        Ok(guard) => guard.provider_cache.clone(),
        Err(error) => {
            tracing::warn!("failed to lock app state for widget snapshot: {error}");
            return;
        }
    };

    let entries: Vec<_> = most_constrained_per_provider(&cached)
        .into_iter()
        .filter_map(widget_entry_from_usage_snapshot)
        .collect();
    if entries.is_empty() {
        return;
    }

    let snapshot = codexbar::core::WidgetSnapshot::new(entries, chrono::Utc::now())
        .with_enabled_providers(enabled_ids.to_vec());
    if let Err(error) = codexbar::core::WidgetSnapshotStore::save(&snapshot) {
        tracing::warn!("failed to save widget snapshot for MCP/statusline: {error}");
    }
}

fn widget_entry_from_usage_snapshot(
    snap: &ProviderUsageSnapshot,
) -> Option<codexbar::core::WidgetProviderEntry> {
    // from_error fills primary with a dummy 0%. MCP/statusline must not
    // publish that as remaining quota (SBS-1061).
    if snap.error.is_some() {
        return None;
    }
    let provider = ProviderId::from_cli_name(&snap.provider_id)?;
    let updated_at = chrono::DateTime::parse_from_rfc3339(&snap.updated_at)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let mut entry = codexbar::core::WidgetProviderEntry::new(provider, updated_at);
    let primary_is_placeholder = snap.inactive_rate_windows.iter().any(|window| {
        matches!(
            window.id.as_str(),
            "cursor-plan" | "cursor-monthly" | "claude-session"
        )
    });
    if !primary_is_placeholder {
        entry = entry.with_primary(rate_window_from_snapshot(&snap.primary));
    }
    if let Some(secondary) = snap.secondary.as_ref() {
        entry = entry.with_secondary(rate_window_from_snapshot(secondary));
    }
    if let Some(tertiary) = snap.tertiary.as_ref() {
        entry = entry.with_tertiary(rate_window_from_snapshot(tertiary));
    }
    if let Some(email) = snap.account_email.clone() {
        entry = entry.with_account_email(email);
    }
    if let Some(plan) = snap.plan_name.clone() {
        entry = entry.with_login_method(plan);
    }
    if let Some(cost) = snap.cost.as_ref() {
        if let Some(remaining) = cost.remaining {
            entry = entry.with_credits_remaining(remaining);
        }
        // Statusline / MCP read period_cost_usd from token_usage. Provider
        // CostSnapshot.used is the billed/current-period dollar figure.
        entry = entry.with_token_usage(
            codexbar::core::TokenUsageSummary::new().with_period(cost.used, cost.period.clone()),
        );
    }
    Some(entry)
}

fn rate_window_from_snapshot(window: &RateWindowSnapshot) -> RateWindow {
    let resets_at = window.resets_at.as_deref().and_then(|raw| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    });
    RateWindow::with_details(
        window.used_percent,
        window.window_minutes,
        resets_at,
        window.reset_description.clone(),
    )
}

fn update_tray_and_notifications(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, Mutex<AppState>>,
    settings: &Settings,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> Result<(), String> {
    let cached = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.provider_cache.clone()
    };
    crate::tray_bridge::update_tray_status_items(app, &cached);
    crate::tray_bridge::update_tray_icon_and_tooltip(app, &cached);
    notify_usage_thresholds(state, settings, token_accounts, &cached);
    Ok(())
}

/// Classify a rate window by its reset cadence into a stable notification key,
/// independent of whether it currently sits in the primary or secondary slot.
///
/// Codex and Claude promote the weekly window into `primary` whenever the API
/// omits the 5-hour meter, so keying alerts by slot makes that swap look like a
/// brand-new threshold crossing — and drives a false session depleted/restored
/// cycle on every refresh. Keying by cadence keeps each real window's alert
/// stable across the swap.
fn window_notify_key(
    provider: ProviderId,
    label: Option<&str>,
    window_minutes: Option<u32>,
) -> &'static str {
    if provider == ProviderId::Cursor
        && label.is_some_and(|label| {
            matches!(
                label.trim().to_ascii_lowercase().as_str(),
                "plan" | "total" | "auto" | "api"
            )
        })
    {
        return "monthly";
    }
    match window_minutes {
        Some(m) if m <= 720 => "session",   // 5-hour class (<= 12h)
        Some(m) if m <= 20_160 => "weekly", // up to two weeks
        Some(_) => "monthly",               // monthly or longer
        None => "primary",                  // unknown cadence — never the session
    }
}

/// Plan per-window threshold alerts for every measured window on a snapshot.
///
/// Returns stable, cadence-based window keys (so a primary/secondary swap cannot
/// masquerade as a new crossing) and the used-percent of the true 5-hour session
/// window *only when it is actually present this refresh* — a promoted weekly is
/// never reported as the session.
///
/// The third and model windows are real subscription ceilings (OpenCode Go
/// Monthly, Claude's Opus pool), so they are offered to the notifier too — which
/// keeps its own policy about what deserves a toast and still declines monthly
/// keys. The model window carries its own key rather than a cadence one:
/// Claude's is 7 days like the weekly window, and a shared key would let
/// whichever came second be dropped as a duplicate.
/// The measured windows of one snapshot, borrowed for alert planning. Grouped
/// so adding a slot does not grow every signature along the way.
struct ThresholdWindows<'a> {
    primary_label: Option<&'a str>,
    primary: &'a RateWindowSnapshot,
    secondary_label: Option<&'a str>,
    secondary: Option<&'a RateWindowSnapshot>,
    tertiary_label: Option<&'a str>,
    tertiary: Option<&'a RateWindowSnapshot>,
    model_specific: Option<&'a RateWindowSnapshot>,
}

impl<'a> ThresholdWindows<'a> {
    fn of(snapshot: &'a ProviderUsageSnapshot) -> Self {
        Self {
            primary_label: snapshot.primary_label.as_deref(),
            primary: &snapshot.primary,
            secondary_label: snapshot.secondary_label.as_deref(),
            secondary: snapshot.secondary.as_ref(),
            tertiary_label: snapshot.tertiary_label.as_deref(),
            tertiary: snapshot.tertiary.as_ref(),
            model_specific: snapshot.model_specific.as_ref(),
        }
    }
}

fn plan_threshold_alerts(
    provider: ProviderId,
    windows: ThresholdWindows<'_>,
) -> (Vec<(&'static str, f64)>, Option<f64>) {
    let mut alerts: Vec<(&'static str, f64)> = Vec::new();
    let mut session_percent: Option<f64> = None;
    let mut seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();

    for (key, window) in std::iter::once((
        window_notify_key(
            provider,
            windows.primary_label,
            windows.primary.window_minutes,
        ),
        windows.primary,
    ))
    .chain(windows.secondary.map(|window| {
        (
            window_notify_key(provider, windows.secondary_label, window.window_minutes),
            window,
        )
    }))
    .chain(windows.tertiary.map(|window| {
        (
            window_notify_key(provider, windows.tertiary_label, window.window_minutes),
            window,
        )
    }))
    .chain(windows.model_specific.map(|window| ("model", window)))
    {
        if key == "session" {
            session_percent = Some(window.used_percent);
        }
        if seen.insert(key) {
            alerts.push((key, window.used_percent));
        }
    }

    (alerts, session_percent)
}

fn notify_usage_thresholds(
    state: &tauri::State<'_, Mutex<AppState>>,
    settings: &Settings,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
    cached: &[ProviderUsageSnapshot],
) {
    let cli_map = codexbar::core::cli_name_map();
    if let Ok(mut guard) = state.lock() {
        for snapshot in cached {
            if snapshot.error.is_none()
                && let Some(&provider) = cli_map.get(snapshot.provider_id.as_str())
            {
                let (alerts, _session_percent) =
                    plan_threshold_alerts(provider, ThresholdWindows::of(snapshot));
                for (window_key, used_percent) in alerts {
                    guard.notification_manager.check_and_notify(
                        provider,
                        snapshot.account_id.as_deref(),
                        window_key,
                        used_percent,
                        settings,
                    );
                }
                // Depleted/restored notifications are deliberately omitted.
                // Confirmed resets provide the useful "quota is back" signal
                // without adding a second, noisier state machine.
                notify_predictive_pace(
                    &mut guard.notification_manager,
                    provider,
                    snapshot,
                    token_accounts,
                    settings,
                );
            }
        }
    }
}

/// The windows a predictive pace warning can be raised for, each already named
/// by its real cadence.
///
/// These are the three slots a provider reports its own quota structure in. A
/// monthly quota lives in the tertiary slot, and leaving that slot out meant a
/// month that was on course to run dry could never say so, while the weekly
/// window beside it warned freely.
///
/// Extra windows are excluded on purpose. The displayed verdict can consider
/// every window because it shows one; here each candidate raises its own toast,
/// and the extras are supplementary lanes (Codex Spark, pooled team usage) that
/// would multiply warnings rather than add information.
fn predictive_pace_candidates<'a>(
    primary: &'a RateWindowSnapshot,
    secondary: Option<&'a RateWindowSnapshot>,
    tertiary: Option<&'a RateWindowSnapshot>,
) -> Vec<(
    codexbar::notifications::PredictiveWarningWindow,
    &'a RateWindowSnapshot,
    u32,
)> {
    use codexbar::notifications::PredictiveWarningWindow as W;
    let mut candidates: Vec<(W, &'a RateWindowSnapshot, u32)> = Vec::with_capacity(3);
    for (fallback_window, window, default_window_minutes) in [
        (W::Session, Some(primary), 300u32),
        (W::Weekly, secondary, 10_080u32),
        (W::Monthly, tertiary, 43_200u32),
    ] {
        let Some(window) = window else {
            continue;
        };
        // Name the window by its real cadence; fall back to the slot's meaning
        // only when the provider does not state a duration.
        let named = window
            .window_minutes
            .map(predictive_window_for)
            .unwrap_or(fallback_window);

        // Notification state is keyed by cadence, so two provider slots with
        // the same cadence must produce one observation. Keep the more-used
        // window to avoid a cooler duplicate suppressing or clearing its alert.
        if let Some(index) = candidates
            .iter()
            .position(|(existing, ..)| *existing == named)
        {
            if window.used_percent > candidates[index].1.used_percent {
                candidates[index] = (named, window, default_window_minutes);
            }
        } else {
            candidates.push((named, window, default_window_minutes));
        }
    }
    candidates
}

fn notify_predictive_pace(
    manager: &mut codexbar::notifications::NotificationManager,
    provider: ProviderId,
    snapshot: &ProviderUsageSnapshot,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
    settings: &Settings,
) {
    let enabled = settings.show_notifications && settings.predictive_pace_warning_enabled;
    manager.set_predictive_warnings_enabled(provider, enabled);
    if !enabled {
        return;
    }

    // Directory seats already stamp `account_id`; threshold toasts key on it.
    // A provider-global token account is only a fallback when no seat is
    // present — using it first made same-email seats share one pace key.
    let directory_account_id = snapshot
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let token_account_id = if directory_account_id.is_some() {
        None
    } else {
        token_accounts
            .get(&provider)
            .and_then(ProviderAccountData::active_account)
            .map(|account| account.id)
    };
    let Some(identity) = predictive_warning_identity(
        &snapshot.source_label,
        snapshot.account_email.as_deref(),
        directory_account_id,
        snapshot.account_organization.as_deref(),
        token_account_id,
    ) else {
        return;
    };
    let observed_at = chrono::DateTime::parse_from_rfc3339(&snapshot.updated_at)
        .ok()
        .map(|date| date.with_timezone(&chrono::Utc));

    for (warning_window, window, default_window_minutes) in predictive_pace_candidates(
        &snapshot.primary,
        snapshot.secondary.as_ref(),
        snapshot.tertiary.as_ref(),
    ) {
        let rate_window = RateWindow::with_details(
            window.used_percent,
            window.window_minutes,
            window
                .resets_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|date| date.with_timezone(&chrono::Utc)),
            window.reset_description.clone(),
        );
        let Some(pace) =
            codexbar::core::UsagePace::weekly(&rate_window, observed_at, default_window_minutes)
        else {
            continue;
        };
        manager.check_predictive_pace(
            provider,
            &identity,
            warning_window,
            &rate_window,
            &pace,
            settings,
        );
    }
}

/// Identifies the account a pace warning belongs to, so a warning fires once
/// per cycle per account rather than once per refresh.
///
/// The dedupe key already carries the provider, so this only has to separate
/// accounts within one. Directory seats use the same `account_id` threshold
/// toasts already key on; organization is included when that id is missing,
/// because two seats can share an email across orgs (SBS-1057). A
/// provider-global token-account UUID is used only when no directory seat
/// is present: it cannot tell those seats apart. Providers that never
/// report an account are still warnable: one unnamed account is still an
/// account.
fn predictive_warning_identity(
    source_label: &str,
    account_email: Option<&str>,
    account_id: Option<&str>,
    organization: Option<&str>,
    token_account_id: Option<uuid::Uuid>,
) -> Option<String> {
    if let Some(account_id) = trimmed_lower(account_id) {
        return Some(format!("account:{account_id}"));
    }
    if let Some(id) = token_account_id {
        return Some(format!("token-account:{}", id.as_hyphenated()));
    }
    let source = source_label.trim().to_ascii_lowercase();
    let email = trimmed_lower(account_email);
    let org = trimmed_lower(organization);
    let mut parts = Vec::new();
    if !source.is_empty() {
        parts.push(source);
    }
    if let Some(email) = email {
        parts.push(email);
    }
    if let Some(org) = org {
        parts.push(format!("org:{org}"));
    }
    (!parts.is_empty()).then(|| parts.join(":"))
}

fn trimmed_lower(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

/// Which cadence a window represents, so the warning names the right thing.
/// Without this a monthly quota would be announced as a "Session" limit.
fn predictive_window_for(window_minutes: u32) -> codexbar::notifications::PredictiveWarningWindow {
    use codexbar::notifications::PredictiveWarningWindow as W;
    match window_minutes {
        0..=720 => W::Session,
        721..=20_160 => W::Weekly,
        _ => W::Monthly,
    }
}

#[tauri::command]
pub async fn refresh_providers(app: tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers(&app).await
}

#[tauri::command]
pub async fn refresh_providers_if_stale(app: tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_if_stale(&app).await
}

#[tauri::command]
pub fn get_cached_providers(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Vec<ProviderUsageSnapshot> {
    let mut snapshots = state
        .lock()
        .map(|guard| guard.provider_cache.clone())
        .unwrap_or_default();
    let spark_usage_visible = Settings::load().codex_spark_usage_visible();
    for snapshot in &mut snapshots {
        super::filter_hidden_codex_spark_rows(snapshot, spark_usage_visible);
    }

    snapshots
}

#[cfg(test)]
mod predictive_warning_tests {
    use super::*;

    #[test]
    fn predictive_warning_identity_keeps_claude_sources_and_token_accounts_separate() {
        let account_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();

        assert_eq!(
            predictive_warning_identity("cli", Some("Person@Example.com"), None, None, None)
                .as_deref(),
            Some("cli:person@example.com")
        );
        assert_eq!(
            predictive_warning_identity("oauth", Some("Person@Example.com"), None, None, None)
                .as_deref(),
            Some("oauth:person@example.com")
        );
        assert_eq!(
            predictive_warning_identity(
                "oauth",
                Some("Person@Example.com"),
                None,
                None,
                Some(account_id)
            )
            .as_deref(),
            Some("token-account:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
    }

    #[test]
    fn predictive_warning_identity_falls_back_to_the_source_alone() {
        // Providers that never report an account still get one stable identity,
        // so they can be warned instead of silently skipped.
        assert_eq!(
            predictive_warning_identity("oauth", None, None, None, None).as_deref(),
            Some("oauth")
        );
        assert_eq!(
            predictive_warning_identity("cli", Some("  "), None, None, None).as_deref(),
            Some("cli")
        );
        assert_eq!(
            predictive_warning_identity("  ", None, None, None, None),
            None
        );
    }

    /// Two directory seats can share a login email (personal vs work org).
    /// After #379 they stamp distinct `account_id`s; threshold toasts key on
    /// those. Pace warnings used to prefer one provider-global token account
    /// or source+email, so both seats shared a key (SBS-1057).
    #[test]
    fn predictive_warning_identity_separates_directory_seats_that_share_an_email() {
        let token_account = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();

        let personal = predictive_warning_identity(
            "oauth",
            Some("Same@Example.com"),
            Some("acct-personal"),
            Some("Personal"),
            Some(token_account),
        );
        let work = predictive_warning_identity(
            "oauth",
            Some("Same@Example.com"),
            Some("acct-work"),
            Some("Acme"),
            Some(token_account),
        );

        assert_eq!(personal.as_deref(), Some("account:acct-personal"));
        assert_eq!(work.as_deref(), Some("account:acct-work"));
        assert_ne!(personal, work);
    }

    #[test]
    fn predictive_warning_identity_uses_organization_when_email_is_shared() {
        // Ambient seats have no directory id. Organization is what keeps a
        // personal workspace from clearing or suppressing a work one.
        let personal = predictive_warning_identity(
            "oauth",
            Some("Same@Example.com"),
            None,
            Some("Personal"),
            None,
        );
        let work = predictive_warning_identity(
            "oauth",
            Some("Same@Example.com"),
            None,
            Some("Acme"),
            None,
        );

        assert_eq!(
            personal.as_deref(),
            Some("oauth:same@example.com:org:personal")
        );
        assert_eq!(work.as_deref(), Some("oauth:same@example.com:org:acme"));
        assert_ne!(personal, work);
    }

    #[test]
    fn predictive_window_is_named_by_its_real_cadence() {
        use codexbar::notifications::PredictiveWarningWindow as W;
        assert_eq!(predictive_window_for(300), W::Session);
        assert_eq!(predictive_window_for(10_080), W::Weekly);
        // A monthly quota must not be announced as a session limit.
        assert_eq!(predictive_window_for(44_640), W::Monthly);
    }

    fn rw(window_minutes: Option<u32>, used_percent: f64) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent,
            remaining_percent: 100.0 - used_percent,
            window_minutes,
            resets_at: None,
            reset_description: None,
            is_exhausted: used_percent >= 100.0,
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    /// Regression: a monthly quota is reported in the tertiary slot, and pace
    /// warnings only ever looked at the primary and secondary. A month on course
    /// to run dry could not say so while the weekly beside it warned freely.
    #[test]
    fn predictive_pace_warns_on_a_monthly_quota() {
        use codexbar::notifications::PredictiveWarningWindow as W;
        let session = rw(Some(300), 10.0);
        let weekly = rw(Some(10_080), 5.0);
        let monthly = rw(Some(43_200), 61.0);

        let candidates = predictive_pace_candidates(&session, Some(&weekly), Some(&monthly));

        let named: Vec<_> = candidates.iter().map(|(window, ..)| *window).collect();
        assert_eq!(named, vec![W::Session, W::Weekly, W::Monthly]);
        assert_eq!(candidates[2].1.used_percent, 61.0);
    }

    #[test]
    fn predictive_pace_skips_slots_a_provider_does_not_report() {
        let session = rw(Some(300), 10.0);

        let candidates = predictive_pace_candidates(&session, None, None);

        assert_eq!(candidates.len(), 1);
    }

    /// The slot's meaning is only a fallback. A provider that states a duration
    /// is named by that, so a monthly window promoted into an earlier slot is
    /// not announced as a session or a week.
    #[test]
    fn predictive_pace_names_a_window_by_its_own_cadence_not_its_slot() {
        use codexbar::notifications::PredictiveWarningWindow as W;
        let monthly_in_primary = rw(Some(43_200), 61.0);
        let undated = rw(None, 61.0);

        let candidates = predictive_pace_candidates(&monthly_in_primary, Some(&undated), None);

        assert_eq!(candidates[0].0, W::Monthly);
        assert_eq!(candidates[1].0, W::Weekly, "no duration falls back to slot");
    }

    #[test]
    fn predictive_pace_dedupes_slots_that_share_a_cadence() {
        use codexbar::notifications::PredictiveWarningWindow as W;
        let session = rw(Some(300), 10.0);
        let weekly = rw(Some(10_080), 40.0);
        let hotter_weekly = rw(Some(10_080), 80.0);

        let candidates = predictive_pace_candidates(&session, Some(&weekly), Some(&hotter_weekly));

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].0, W::Weekly);
        assert_eq!(candidates[1].1.used_percent, 80.0);

        let monthly = rw(Some(43_200), 40.0);
        let hotter_monthly = rw(Some(43_200), 80.0);
        let candidates =
            predictive_pace_candidates(&session, Some(&monthly), Some(&hotter_monthly));

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].0, W::Monthly);
        assert_eq!(candidates[1].1.used_percent, 80.0);
    }

    #[test]
    fn window_key_classifies_by_reset_cadence() {
        assert_eq!(
            window_notify_key(ProviderId::Claude, None, Some(300)),
            "session"
        );
        assert_eq!(
            window_notify_key(ProviderId::Claude, None, Some(10_080)),
            "weekly"
        );
        assert_eq!(
            window_notify_key(ProviderId::Claude, None, Some(43_200)),
            "monthly"
        );
        assert_eq!(window_notify_key(ProviderId::Claude, None, None), "primary");
        assert_eq!(
            window_notify_key(ProviderId::Cursor, Some("Plan"), Some(300)),
            "monthly",
            "Cursor's billing plan must never be described as a session"
        );
    }

    /// Regression: Codex's API intermittently omits the 5-hour meter, so the app
    /// promotes the weekly window into `primary`. Keying by slot made every swap
    /// look like a new crossing and a session depleted/restored cycle. Keying by
    /// cadence must keep the weekly stable and never read it as the session.
    #[test]
    fn promoted_weekly_is_not_treated_as_session() {
        // 5-hour present: primary = 5h (100%), secondary = weekly (75%).
        let (alerts, session) = plan_threshold_alerts(
            ProviderId::Codex,
            ThresholdWindows {
                primary_label: Some("Session"),
                primary: &rw(Some(300), 100.0),
                secondary_label: Some("Weekly"),
                secondary: Some(&rw(Some(10_080), 75.0)),
                tertiary_label: None,
                tertiary: None,
                model_specific: None,
            },
        );
        assert_eq!(session, Some(100.0));
        assert!(alerts.contains(&("session", 100.0)));
        assert!(alerts.contains(&("weekly", 75.0)));

        // Next refresh omits the 5-hour, so the weekly is promoted to primary.
        // It must NOT be reported as the session, and it keeps the "weekly" key.
        let (alerts, session) = plan_threshold_alerts(
            ProviderId::Codex,
            ThresholdWindows {
                primary_label: Some("Weekly"),
                primary: &rw(Some(10_080), 75.0),
                secondary_label: None,
                secondary: None,
                tertiary_label: None,
                tertiary: None,
                model_specific: None,
            },
        );
        assert_eq!(
            session, None,
            "a promoted weekly must not be read as the session"
        );
        assert_eq!(alerts, vec![("weekly", 75.0)]);
    }

    /// The third and model windows are real ceilings. They were never handed to
    /// the notifier, so an OpenCode Go monthly pool or a Claude Opus pool could
    /// sit at 99% without a word.
    #[test]
    fn third_and_model_windows_alert_too() {
        let (alerts, _) = plan_threshold_alerts(
            ProviderId::OpenCodeGo,
            ThresholdWindows {
                primary_label: Some("Rolling (5h)"),
                primary: &rw(Some(300), 34.0),
                secondary_label: Some("Weekly"),
                secondary: Some(&rw(Some(10_080), 32.0)),
                tertiary_label: Some("Monthly"),
                tertiary: Some(&rw(Some(43_200), 97.0)),
                model_specific: None,
            },
        );
        assert!(alerts.contains(&("monthly", 97.0)));

        // Claude's model pool runs on the same 7-day cadence as its weekly
        // window, so a cadence key would collide and the second one would be
        // dropped as a duplicate. The model window carries its own key.
        let (alerts, _) = plan_threshold_alerts(
            ProviderId::Claude,
            ThresholdWindows {
                primary_label: Some("Session (5h)"),
                primary: &rw(Some(300), 4.0),
                secondary_label: Some("Weekly"),
                secondary: Some(&rw(Some(10_080), 20.0)),
                tertiary_label: None,
                tertiary: None,
                model_specific: Some(&rw(Some(10_080), 96.0)),
            },
        );
        assert!(alerts.contains(&("weekly", 20.0)));
        assert!(alerts.contains(&("model", 96.0)));
    }

    /// A payload for eligibility checks only; the numbers do not matter.
    #[cfg(test)]
    fn eligibility_event(
        kind: crate::capacity_events::CapacityEventKind,
        window_id: &str,
        window_minutes: Option<u32>,
    ) -> crate::capacity_events::CapacityEventPayload {
        crate::capacity_events::CapacityEventPayload {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            window_id: window_id.into(),
            window_label: window_id.into(),
            window_minutes,
            kind,
            previous_used_percent: 90.0,
            current_used_percent: 2.0,
            previous_reset_credits: None,
            current_reset_credits: None,
            previous_reset_at: "2026-07-25T03:42:20Z".into(),
            current_reset_at: "2026-08-01T03:42:20Z".into(),
            occurred_at: "2026-07-25T03:42:20Z".into(),
            while_away: false,
        }
    }

    #[test]
    fn only_confirmed_resets_are_eligible_for_windows_notifications() {
        use crate::capacity_events::CapacityEventKind;

        for kind in [
            CapacityEventKind::ScheduledReset,
            CapacityEventKind::SurpriseReset,
            CapacityEventKind::PartialReset,
            CapacityEventKind::BankedResetGranted,
        ] {
            assert!(capacity_event_uses_windows_notification(
                &eligibility_event(kind, "weekly", Some(10_080))
            ));
        }
        for visual_only in [
            CapacityEventKind::ResetTimeShift,
            CapacityEventKind::WindowLifted,
            CapacityEventKind::WindowRestored,
            CapacityEventKind::AllowanceGranted,
        ] {
            assert!(!capacity_event_uses_windows_notification(
                &eligibility_event(visual_only, "weekly", Some(10_080))
            ));
        }
    }

    #[test]
    fn a_scheduled_five_hour_reset_is_not_worth_a_toast() {
        use crate::capacity_events::CapacityEventKind;

        assert!(!capacity_event_uses_windows_notification(
            &eligibility_event(CapacityEventKind::ScheduledReset, "session", Some(300))
        ));
        // Providers that omit a cadence fall back to the semantic window id.
        assert!(!capacity_event_uses_windows_notification(
            &eligibility_event(CapacityEventKind::ScheduledReset, "session", None)
        ));

        // The long windows a user actually plans around must still notify,
        // including provider-specific ids like Cursor's monthly "Plan" pool.
        for (window_id, minutes) in [
            ("weekly", Some(10_080)),
            ("monthly", Some(43_200)),
            ("plan", None),
        ] {
            assert!(
                capacity_event_uses_windows_notification(&eligibility_event(
                    CapacityEventKind::ScheduledReset,
                    window_id,
                    minutes
                )),
                "{window_id} reset should notify"
            );
        }
    }

    #[test]
    fn an_unexpected_session_reset_still_notifies() {
        use crate::capacity_events::CapacityEventKind;

        // Getting a 5-hour window back early is not something the user could
        // have predicted, so the noise argument for scheduled resets does not
        // apply.
        for kind in [
            CapacityEventKind::SurpriseReset,
            CapacityEventKind::PartialReset,
        ] {
            assert!(capacity_event_uses_windows_notification(
                &eligibility_event(kind, "session", Some(300))
            ));
        }
    }

    #[test]
    fn simultaneous_partial_resets_are_batched_into_one_notification() {
        use crate::capacity_events::{CapacityEventKind, CapacityEventPayload};

        let event =
            |window_id: &str, window_label: &str, before: f64, after: f64| CapacityEventPayload {
                provider_id: "cursor".into(),
                display_name: "Cursor".into(),
                window_id: window_id.into(),
                window_label: window_label.into(),
                window_minutes: Some(43_200),
                kind: CapacityEventKind::PartialReset,
                previous_used_percent: before,
                current_used_percent: after,
                previous_reset_credits: None,
                current_reset_credits: None,
                previous_reset_at: "2026-08-06T22:49:57Z".into(),
                current_reset_at: "2026-08-06T22:49:57Z".into(),
                occurred_at: "2026-07-15T20:21:45Z".into(),
                while_away: false,
            };
        let events = vec![
            event("auto", "Auto", 99.4, 49.7),
            event("plan", "Plan", 85.25, 48.19),
        ];

        let (title, body) = capacity_event_notification(&events).unwrap();
        assert_eq!(title, "Cursor capacity restored");
        assert_eq!(body, "Auto 99% → 50% used; Plan 85% → 48% used.");
    }

    #[test]
    fn batched_banked_resets_pluralize_without_disturbing_window_formatting() {
        use crate::capacity_events::{CapacityEventKind, CapacityEventPayload};

        let partial = CapacityEventPayload {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            window_id: "weekly".into(),
            window_label: "Weekly".into(),
            window_minutes: Some(10_080),
            kind: CapacityEventKind::PartialReset,
            previous_used_percent: 90.0,
            current_used_percent: 10.0,
            previous_reset_credits: None,
            current_reset_credits: None,
            previous_reset_at: "2026-07-25T03:42:20Z".into(),
            current_reset_at: "2026-07-25T03:42:20Z".into(),
            occurred_at: "2026-07-18T03:25:06Z".into(),
            while_away: false,
        };
        let banked = CapacityEventPayload {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            window_id: "banked-resets".into(),
            window_label: "Banked resets".into(),
            window_minutes: None,
            kind: CapacityEventKind::BankedResetGranted,
            previous_used_percent: 0.0,
            current_used_percent: 0.0,
            previous_reset_credits: Some(1),
            current_reset_credits: Some(2),
            previous_reset_at: "2026-07-18T03:20:05Z".into(),
            current_reset_at: "2026-07-18T03:25:06Z".into(),
            occurred_at: "2026-07-18T03:25:06Z".into(),
            while_away: false,
        };

        let (title, body) = capacity_event_notification(&[partial, banked]).unwrap();
        assert_eq!(title, "Codex capacity restored");
        assert_eq!(body, "Weekly 90% → 10% used; 2 banked resets available.");
    }
}

#[cfg(test)]
mod widget_snapshot_tests {
    use super::*;
    use codexbar::core::{CostSnapshot, RateWindow, UsageSnapshot};

    /// SBS-1031: CostSnapshot.used is billed/period spend, not a conversation.
    #[test]
    fn widget_entry_stores_period_cost_from_cost_snapshot() {
        let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
        let result = ProviderFetchResult::new(UsageSnapshot::new(RateWindow::new(42.0)), "oauth")
            .with_cost(CostSnapshot::new(12.5, "USD", "Monthly").with_limit(100.0));
        let snap = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
        let entry = widget_entry_from_usage_snapshot(&snap).expect("entry");
        let usage = entry.token_usage.expect("token usage");
        assert_eq!(usage.period_cost_usd, Some(12.5));
        assert_eq!(usage.cost_period.as_deref(), Some("Monthly"));
        assert_eq!(entry.credits_remaining, Some(87.5));
    }

    #[test]
    fn widget_entry_omits_an_unavailable_claude_session_placeholder() {
        let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
        let usage = UsageSnapshot::new(RateWindow::new(0.0)).with_unavailable_rate_window(
            "claude-session",
            "Session (5h)",
            "No usage reported",
        );
        let result = ProviderFetchResult::new(usage, "oauth");
        let snap = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

        let entry = widget_entry_from_usage_snapshot(&snap).expect("entry");
        assert!(entry.primary.is_none());
    }

    /// SBS-1061: from_error still carries a dummy 0% primary. MCP/statusline
    /// must not publish that as a reading.
    #[test]
    fn widget_snapshot_omits_an_errored_vertex_zero_percent() {
        let metadata = instantiate_provider(ProviderId::VertexAI)
            .metadata()
            .clone();
        let snap = ProviderUsageSnapshot::from_error(
            ProviderId::VertexAI,
            &metadata,
            "Vertex AI Resource Manager request failed: HTTP 500".to_string(),
        );
        assert!(snap.error.is_some());
        assert_eq!(snap.primary.used_percent, 0.0);

        assert!(
            most_constrained_per_provider(std::slice::from_ref(&snap)).is_empty(),
            "errored Vertex must not win a widget/MCP slot"
        );
        // If ranking ever included it, the entry writer still must not emit 0%.
        if let Some(entry) = widget_entry_from_usage_snapshot(&snap) {
            panic!(
                "errored Vertex must not become a widget entry (primary {:?})",
                entry.primary.as_ref().map(|w| w.used_percent)
            );
        }
    }

    #[test]
    fn widget_entry_keeps_a_measured_zero_percent_primary() {
        let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
        let result = ProviderFetchResult::new(UsageSnapshot::new(RateWindow::new(0.0)), "oauth");
        let snap = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

        let entry = widget_entry_from_usage_snapshot(&snap).expect("entry");
        assert_eq!(entry.primary.expect("measured primary").used_percent, 0.0);
    }
}
