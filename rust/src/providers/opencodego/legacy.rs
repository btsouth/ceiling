//! Legacy `/workspace/<id>/go` scraper.
//!
//! The original OpenCode Go integration, kept as the fallback for accounts
//! that still carry a legacy `auth` cookie and have not migrated to the
//! `console.opencode.ai`-style surface that [`super::console`] reads. Only
//! used when [`super::console`] fails with a recoverable error and a legacy
//! cookie is present.

use chrono::{DateTime, Utc};
use reqwest::Client;
use std::time::Duration;
use uuid::Uuid;

use crate::core::{ProviderError, RateWindow, UsageSnapshot};

use super::{BASE_URL, USER_AGENT};

const SERVER_URL: &str = "https://opencode.ai/_server";
const WORKSPACES_SERVER_ID: &str =
    "def39973159c7f0483d8793a822b8dbb10d067e12c65455fcb4608459ba0234f";
const BILLING_SERVER_ID: &str = "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d";
const BILLING_SCALE: f64 = 100_000_000.0;

/// Parsed legacy usage response returned to the provider orchestrator.
pub(super) struct LegacyUsage {
    pub(super) usage: UsageSnapshot,
    pub(super) embedded_balance: Option<f64>,
}

pub(super) async fn discover_workspace_id(
    client: &Client,
    cookie_header: &str,
) -> Result<String, ProviderError> {
    let text = fetch_server_text(
        client,
        cookie_header,
        WORKSPACES_SERVER_ID,
        None,
        BASE_URL,
        None,
        "workspace API",
    )
    .await?;
    parse_workspace_ids(&text)
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Parse("No workspace ID found".to_string()))
}

pub(super) async fn fetch_usage(
    client: &Client,
    cookie_header: &str,
    workspace_id: &str,
) -> Result<LegacyUsage, ProviderError> {
    let url = format!("{BASE_URL}/workspace/{workspace_id}/go");
    let page = fetch_page_text(client, cookie_header, &url, None, "usage page").await?;
    parse_page(&page)
}

/// Pure parse of an already-fetched usage page. Split out so tests do not
/// need a network round trip.
fn parse_page(page: &str) -> Result<LegacyUsage, ProviderError> {
    Ok(LegacyUsage {
        usage: parse_usage_text(page)?,
        embedded_balance: parse_zen_balance(page),
    })
}

/// Fallback balance lookup for pages that do not embed the Zen balance.
/// Optional enrichment: callers treat any error here as "no balance",
/// never as a fetch failure.
pub(super) async fn fetch_balance(
    client: &Client,
    cookie_header: &str,
    workspace_id: &str,
    timeout: Duration,
) -> Result<Option<f64>, ProviderError> {
    let referer = format!("{BASE_URL}/workspace/{workspace_id}");
    let page = fetch_page_text(
        client,
        cookie_header,
        &referer,
        Some(timeout),
        "Zen dashboard page",
    )
    .await?;
    if let Some(balance) = parse_zen_balance(&page) {
        return Ok(Some(balance));
    }

    let args = serde_json::json!([workspace_id]).to_string();
    let billing = fetch_server_text(
        client,
        cookie_header,
        BILLING_SERVER_ID,
        Some(&args),
        &referer,
        Some(timeout),
        "billing API",
    )
    .await?;
    Ok(parse_billing_server_balance(&billing))
}

async fn fetch_page_text(
    client: &Client,
    cookie_header: &str,
    url: &str,
    timeout: Option<Duration>,
    what: &str,
) -> Result<String, ProviderError> {
    let mut request = client
        .get(url)
        .header("Cookie", cookie_header)
        .header("User-Agent", USER_AGENT)
        .header("Referer", BASE_URL)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        );
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ProviderError::AuthRequired);
        }
        return Err(ProviderError::Other(format!(
            "OpenCode Go {what} returned {status}"
        )));
    }
    let text = response.text().await?;
    if looks_signed_out(&text) {
        return Err(ProviderError::AuthRequired);
    }
    Ok(text)
}

async fn fetch_server_text(
    client: &Client,
    cookie_header: &str,
    server_id: &str,
    args: Option<&str>,
    referer: &str,
    timeout: Option<Duration>,
    what: &str,
) -> Result<String, ProviderError> {
    let mut url = reqwest::Url::parse(SERVER_URL)
        .map_err(|error| ProviderError::Parse(format!("Invalid OpenCode server URL: {error}")))?;
    url.query_pairs_mut().append_pair("id", server_id);
    if let Some(args) = args {
        url.query_pairs_mut().append_pair("args", args);
    }
    let mut request = client
        .get(url)
        .header("Cookie", cookie_header)
        .header("X-Server-Id", server_id)
        .header("X-Server-Instance", format!("server-fn:{}", Uuid::new_v4()))
        .header("User-Agent", USER_AGENT)
        .header("Origin", BASE_URL)
        .header("Referer", referer)
        .header(
            "Accept",
            "text/javascript, application/json;q=0.9, */*;q=0.8",
        );
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ProviderError::AuthRequired);
        }
        return Err(ProviderError::Other(format!(
            "OpenCode Go {what} returned {status}"
        )));
    }
    let text = response.text().await?;
    if looks_signed_out(&text) {
        return Err(ProviderError::AuthRequired);
    }
    Ok(text)
}

fn parse_workspace_ids(text: &str) -> Vec<String> {
    let pattern = r#"(wrk_[A-Za-z0-9_-]+)"#;
    let re = match regex_lite::Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let mut seen = Vec::new();
    for caps in re.captures_iter(text) {
        if let Some(m) = caps.get(1) {
            let s = m.as_str().to_string();
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
    }
    seen
}

fn looks_signed_out(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("auth/authorize")
        || lower.contains("\"signin\"")
        || lower.contains("please sign in")
}

fn parse_usage_text(text: &str) -> Result<UsageSnapshot, ProviderError> {
    let now = Utc::now();

    let raw_rolling = extract_window(text, &["rollingUsage", "rolling_usage", "rolling"])
        .ok_or_else(|| ProviderError::Parse("Missing rolling usage window".to_string()))?;
    let raw_weekly = extract_window(text, &["weeklyUsage", "weekly_usage", "weekly"]);
    let raw_monthly = extract_window(text, &["monthlyUsage", "monthly_usage", "monthly"]);

    // The page reports every window on one scale, either whole percentages
    // (`23` = 23%) or fractions of the limit (`0.23` = 23%). A lone `1` is
    // ambiguous, so resolve the scale from the whole response before
    // converting any window.
    let fraction_scale = crate::core::detect_fraction_scale(
        std::iter::once(raw_rolling.0)
            .chain(raw_weekly.map(|window| window.0))
            .chain(raw_monthly.map(|window| window.0)),
    );

    let rolling = crate::core::to_percent(raw_rolling.0, fraction_scale);
    let weekly =
        raw_weekly.map(|window| (crate::core::to_percent(window.0, fraction_scale), window.1));
    let monthly =
        raw_monthly.map(|window| (crate::core::to_percent(window.0, fraction_scale), window.1));

    let primary = RateWindow::with_details(
        rolling,
        Some(300),
        raw_rolling
            .1
            .map(|secs| now + chrono::Duration::seconds(secs)),
        None,
    );
    let mut snap = UsageSnapshot::new(primary).with_login_method("OpenCode Go");

    if let Some((pct, reset)) = weekly {
        snap = snap.with_secondary(RateWindow::with_details(
            pct,
            Some(10080),
            reset.map(|secs| now + chrono::Duration::seconds(secs)),
            None,
        ));
    }

    if let Some((pct, reset)) = monthly {
        snap = snap
            .with_tertiary(RateWindow::with_details(
                pct,
                Some(43200),
                reset.map(|secs| now + chrono::Duration::seconds(secs)),
                None,
            ))
            .with_tertiary_label("Monthly");
    }

    if let Some(renews_at) = extract_renewal(text) {
        snap = snap.with_extra_rate_window(
            "renewal",
            "Renews",
            RateWindow::with_details(0.0, None, Some(renews_at), None),
        );
    }

    Ok(snap)
}

/// Extract raw `(usage, resetInSec)` for a usage block by name, before any
/// percent-scale normalization.
///
/// Reset is `None` when the percent matched but no reset field did. Callers
/// must not invent a `resets_at` of "now" for that case — that is a claim
/// about schedule, not an unknown.
fn extract_window(text: &str, names: &[&str]) -> Option<(f64, Option<i64>)> {
    for name in names {
        let percent_pattern = format!(
            r#"{}[^}}]*?(?:usagePercent|usedPercent|percentUsed|percent)\s*[:=]\s*([0-9]+(?:\.[0-9]+)?)"#,
            name
        );
        let reset_pattern = format!(
            r#"{}[^}}]*?(?:resetInSec|resetInSeconds|resetSeconds|resetSec)\s*[:=]\s*([0-9]+)"#,
            name
        );

        let percent = extract_number(&percent_pattern, text);
        if let Some(p) = percent {
            let reset = extract_number(&reset_pattern, text).map(|n| (n as i64).max(0));
            return Some((p, reset));
        }
    }
    None
}

fn extract_number(pattern: &str, text: &str) -> Option<f64> {
    let re = regex_lite::Regex::new(pattern).ok()?;
    re.captures(text)?.get(1)?.as_str().parse().ok()
}

fn extract_renewal(text: &str) -> Option<DateTime<Utc>> {
    let re = regex_lite::Regex::new(
        r#"(?:"renewAt"|"renew_at"|renewAt|renew_at)\s*[:=]\s*"?([^",}\s]+)"?"#,
    )
    .ok()?;
    let raw = re.captures(text)?.get(1)?.as_str();
    date_from_text(raw)
}

fn date_from_text(raw: &str) -> Option<DateTime<Utc>> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(number) = text.parse::<f64>() {
        return date_from_timestamp(number);
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn date_from_timestamp(number: f64) -> Option<DateTime<Utc>> {
    if !number.is_finite() || number <= 0.0 {
        return None;
    }
    let seconds = if number > 10_000_000_000.0 {
        number / 1000.0
    } else {
        number
    };
    DateTime::<Utc>::from_timestamp(seconds as i64, 0)
}

fn parse_zen_balance(text: &str) -> Option<f64> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(value) = find_balance_value(&json)
    {
        return Some(value);
    }
    let patterns = [
        r#"(?i)(?:current\s+balance|zen\s+balance|現在の残高)[^$]{0,80}\$\s*([0-9][0-9,]*(?:\.[0-9]+)?)"#,
        r#"(?i)(?:balance|残高)[\s\S]{0,120}?\$\s*([0-9][0-9,]*(?:\.[0-9]+)?)"#,
    ];
    patterns.iter().find_map(|pattern| {
        let re = regex_lite::Regex::new(pattern).ok()?;
        let raw = re.captures(text)?.get(1)?.as_str().replace(',', "");
        raw.parse::<f64>().ok()
    })
}

fn find_balance_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let normalized: String = key
                    .to_lowercase()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect();
                if matches!(
                    normalized.as_str(),
                    "zenbalance"
                        | "zencurrentbalance"
                        | "currentbalance"
                        | "currentbalanceusd"
                        | "balanceusd"
                        | "usdbalance"
                ) {
                    if let Some(number) = value.as_f64() {
                        return Some(number);
                    }
                    if let Some(text) = value.as_str()
                        && let Ok(number) = text.trim().replace(',', "").parse()
                    {
                        return Some(number);
                    }
                }
                if let Some(found) = find_balance_value(value) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_balance_value),
        _ => None,
    }
}

fn parse_billing_server_balance(text: &str) -> Option<f64> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(raw) = find_raw_billing_balance(&json)
    {
        return Some(raw / BILLING_SCALE);
    }
    let customer_re = regex_lite::Regex::new(
        r#"(?:\"customerID\"|customerID)\s*:\s*(?:\$R\[\d+\]\s*=\s*)?\"[^\"]+\""#,
    )
    .ok()?;
    customer_re.find(text)?;
    let balance_re = regex_lite::Regex::new(
        r#"(?:\"balance\"|balance)\s*:\s*(?:\$R\[\d+\]\s*=\s*)?(-?[0-9]+(?:\.[0-9]+)?)"#,
    )
    .ok()?;
    let raw: f64 = balance_re
        .captures(text)?
        .get(1)?
        .as_str()
        .replace(',', "")
        .parse()
        .ok()?;
    Some(raw / BILLING_SCALE)
}

fn find_raw_billing_balance(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(balance) = map.get("balance") {
                let customer_ok = map
                    .get("customerID")
                    .and_then(|value| value.as_str())
                    .is_some_and(|id| !id.is_empty());
                if !customer_ok {
                    return None;
                }
                return billing_numeric_value(balance);
            }
            map.values().find_map(find_raw_billing_balance)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_raw_billing_balance),
        _ => None,
    }
}

fn billing_numeric_value(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_ids_without_duplicates() {
        let text = r#"{ id: "wrk_abc123", name: "x" } { id: "wrk_def456" } { id: "wrk_abc123" }"#;
        assert_eq!(
            parse_workspace_ids(text),
            vec!["wrk_abc123".to_string(), "wrk_def456".to_string()]
        );
    }

    #[test]
    fn parses_usage_blocks_as_whole_percentages() {
        let text = r#"
            rollingUsage: { usagePercent: 42.5, resetInSec: 3600 }
            weeklyUsage: { usagePercent: 13, resetInSec: 86400 }
            monthlyUsage: { usagePercent: 7, resetInSec: 2592000 }
        "#;
        let snap = parse_usage_text(text).unwrap();
        assert!((snap.primary.used_percent - 42.5).abs() < 0.001);
        let secondary = snap.secondary.expect("weekly");
        assert!((secondary.used_percent - 13.0).abs() < 0.001);
        let tertiary = snap.tertiary.expect("monthly");
        assert!((tertiary.used_percent - 7.0).abs() < 0.001);
        assert_eq!(snap.tertiary_label.as_deref(), Some("Monthly"));
    }

    #[test]
    fn parses_usage_blocks_as_fractions() {
        let text = r#"
            rollingUsage: { usagePercent: 0.425, resetInSec: 3600 }
            weeklyUsage: { usagePercent: 0.13, resetInSec: 86400 }
            monthlyUsage: { usagePercent: 0.07, resetInSec: 2592000 }
        "#;
        let snap = parse_usage_text(text).unwrap();
        assert!((snap.primary.used_percent - 42.5).abs() < 0.001);
        let secondary = snap.secondary.expect("weekly");
        assert!((secondary.used_percent - 13.0).abs() < 0.001);
        let tertiary = snap.tertiary.expect("monthly");
        assert!((tertiary.used_percent - 7.0).abs() < 0.001);
    }

    #[test]
    fn one_percent_window_is_not_full() {
        // The reported regression: a lightly used account sent `1` for its
        // rolling window (1% on the whole-percent scale) with the other
        // windows at `0`. The old per-window rule read that lone `1` as a
        // fraction and rendered the rolling window as 100% used.
        let text = r#"
            rollingUsage: { usagePercent: 1, resetInSec: 3600 }
            weeklyUsage: { usagePercent: 0, resetInSec: 86400 }
            monthlyUsage: { usagePercent: 0, resetInSec: 2592000 }
        "#;
        let snap = parse_usage_text(text).unwrap();
        assert!((snap.primary.used_percent - 1.0).abs() < 0.001);
        let secondary = snap.secondary.expect("weekly");
        assert!((secondary.used_percent - 0.0).abs() < 0.001);
        let tertiary = snap.tertiary.expect("monthly");
        assert!((tertiary.used_percent - 0.0).abs() < 0.001);
    }

    #[test]
    fn parses_renewal_window() {
        let text = r#"
            rollingUsage: { usagePercent: 42.5, resetInSec: 3600 }
            weeklyUsage: { usagePercent: 50, resetInSec: 86400 }
            renewAt: "2026-06-01T12:00:00Z"
        "#;
        let snap = parse_usage_text(text).unwrap();
        let renewal = snap
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "renewal")
            .expect("renewal window");
        assert_eq!(renewal.title, "Renews");
        assert_eq!(
            renewal.window.resets_at.unwrap().to_rfc3339(),
            "2026-06-01T12:00:00+00:00"
        );
    }

    #[test]
    fn missing_reset_field_does_not_claim_resets_now() {
        // Percent matched, reset key omitted → schedule is unknown. Substituting
        // 0s would set resets_at to the fetch timestamp and poison pace/reset
        // detection with a fake "resets this instant" claim.
        let text = r#"
            rollingUsage: { usagePercent: 42.5 }
            weeklyUsage: { usagePercent: 13, resetInSec: 86400 }
        "#;
        let snap = parse_usage_text(text).unwrap();
        assert!(
            snap.primary.resets_at.is_none(),
            "unknown reset must stay None, got {:?}",
            snap.primary.resets_at
        );
        let secondary = snap.secondary.expect("weekly still parses with reset");
        assert!(secondary.resets_at.is_some());
        assert!((snap.primary.used_percent - 42.5).abs() < 0.001);
    }

    #[test]
    fn embedded_zen_balance_is_parsed_from_the_page() {
        let page = r#"{rollingUsage: {usedPercent: 40, resetInSec: 3600}} Current balance $12.50"#;
        let result = parse_page(page).expect("usage");
        assert_eq!(result.embedded_balance, Some(12.50));
    }

    #[test]
    fn billing_server_balance_needs_customer_marker() {
        assert_eq!(
            parse_billing_server_balance(r#"{"balance": 1500000000, "customerID": "cus_123"}"#),
            Some(15.0)
        );
        assert_eq!(
            parse_billing_server_balance(r#"{"balance": 1500000000}"#),
            None
        );
        assert_eq!(
            parse_billing_server_balance(r#"{"balance": true, "customerID": "cus_123"}"#),
            None
        );
    }

    #[test]
    fn billing_server_balance_handles_nesting_and_rsc_fragments() {
        assert_eq!(
            parse_billing_server_balance(r#"{"balance": "1,000,000,000", "customerID": "cus_1"}"#),
            Some(10.0)
        );
        assert_eq!(
            parse_billing_server_balance(
                r#"{"data": {"rows": [{"balance": 250000000, "customerID": "cus_2"}]}}"#
            ),
            Some(2.5)
        );
        assert_eq!(
            parse_billing_server_balance(r#"customerID:$R[1] = "cus_9"; "balance": -500000000"#),
            Some(-5.0)
        );
        assert_eq!(parse_billing_server_balance(r#"customerID: "cus_9""#), None);
    }
}
