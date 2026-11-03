//! Cursor API client for fetching usage information
//!
//! Uses browser cookies to authenticate with cursor.com API. Plan data comes from
//! `usage-summary`; Grok Bot and X Subscription data come from `api/dashboard/*`.

#[cfg(test)]
use crate::core::EnforcementState;
use crate::core::{
    CostSnapshot, InactiveRateWindow, NamedRateWindow, ProviderError, RateWindow, UsageSnapshot,
    WindowAmount,
};
use crate::providers::browser_cookie_header;
use chrono::{DateTime, Utc};
use serde::Deserialize;

const BASE_URL: &str = "https://cursor.com";
const COOKIE_DOMAINS: [&str; 2] = ["cursor.com", "cursor.sh"];
const NOT_ENFORCED: &str = "Not currently enforced by Cursor";
const ON_DEMAND_ID: &str = "cursor-on-demand";
const ON_DEMAND_TITLE: &str = "On-demand";
const ON_DEMAND_PERIOD: &str = "On-demand";
/// Plan / overall / pooled cents are Cursor internal units, not an invoice.
const INCLUDED_PERIOD: &str = "Included";
const NO_USAGE_REPORTED: &str = "No usage reported";
const API_ID: &str = "cursor-api";
const API_TITLE: &str = "API";
const GROK_BOT_ID: &str = "cursor-grok-bot";
const GROK_BOT_TITLE: &str = "Grok Bot";
const WEEKLY_MINUTES: u32 = 7 * 24 * 60;
/// Prepaid credits are what an X Subscription spends past its weekly pool.
const CREDITS_PERIOD: &str = "Credits";
const DASHBOARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Matches `usage-summary`'s own limit, so this extra data never slows a refresh
/// beyond what the plan data can already take.
const GROK_BOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub(super) type CursorUsageResult = (UsageSnapshot, Option<CostSnapshot>);

/// Cursor API client
pub struct CursorApi {
    client: reqwest::Client,
}

impl CursorApi {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Fetch usage information from Cursor API
    pub async fn fetch_usage(&self) -> Result<CursorUsageResult, ProviderError> {
        let cookie_header = self.get_cookie_header()?;
        self.fetch_usage_with_cookie_header(&cookie_header).await
    }

    /// Fetch usage information with an already resolved Cookie header.
    pub async fn fetch_usage_with_cookie_header(
        &self,
        cookie_header: &str,
    ) -> Result<CursorUsageResult, ProviderError> {
        let (usage_result, user_result, grok_bot) = tokio::join!(
            self.fetch_usage_summary(cookie_header),
            self.fetch_user_info(cookie_header),
            async {
                tokio::time::timeout(GROK_BOT_TIMEOUT, self.fetch_grok_bot(cookie_header))
                    .await
                    .unwrap_or(None)
            }
        );

        let usage_summary = usage_result?;
        let user_info = user_result.ok();

        let (mut usage, mut cost) = self.build_result(usage_summary, user_info)?;
        if let Some(reading) = grok_bot {
            apply_grok_bot(&mut usage, &mut cost, &reading);
        }
        Ok((usage, cost))
    }

    /// Read Grok Bot's meter the same way Grok Bot does. Best effort: Cursor's
    /// plan data stands on its own, so a failure here only leaves it unchanged.
    async fn fetch_grok_bot(&self, cookie_header: &str) -> Option<GrokBotReading> {
        let status: SandUsageStatus = self
            .dashboard_post(
                cookie_header,
                "get-sand-usage-status",
                serde_json::json!({}),
            )
            .await
            .map_err(|error| tracing::debug!("Cursor Grok Bot status unavailable: {error}"))
            .ok()?;
        status.reading()?;

        if status.is_unified {
            let (period, prepaid) = tokio::join!(
                self.dashboard_post::<CurrentPeriodUsage>(
                    cookie_header,
                    "get-current-period-usage",
                    serde_json::json!({}),
                ),
                self.dashboard_post::<UserPrepaidBilling>(
                    cookie_header,
                    "get-user-prepaid-billing",
                    serde_json::json!({}),
                ),
            );
            return Some(GrokBotReading {
                status,
                period: period.ok(),
                credits_spent_cents: prepaid.ok().and_then(|billing| billing.spent_cents()),
                on_demand_cents: None,
            });
        }

        let on_demand_cents = self.fetch_grok_bot_on_demand_cents(cookie_header).await;
        Some(GrokBotReading {
            status,
            period: None,
            credits_spent_cents: None,
            on_demand_cents,
        })
    }

    /// Grok Bot's share of on-demand spend this billing month. Grok Bot's own
    /// usage screen asks for this exact client and product.
    async fn fetch_grok_bot_on_demand_cents(&self, cookie_header: &str) -> Option<i64> {
        let cycle: MonthlyBillingCycle = self
            .dashboard_post(
                cookie_header,
                "get-monthly-billing-cycle",
                serde_json::json!({}),
            )
            .await
            .ok()?;
        let spend: DailySpendByCategory = self
            .dashboard_post(
                cookie_header,
                "get-daily-spend-by-category",
                serde_json::json!({
                    "periodStartMs": cycle.start_date_epoch_millis,
                    "periodEndMs": cycle.end_date_epoch_millis,
                    "groupBy": "SPEND_GROUP_BY_CATEGORY_MODEL",
                    "spendType": "SPEND_TYPE_ON_DEMAND",
                    "clientType": "grok-bot",
                    "products": ["sand"],
                }),
            )
            .await
            .ok()?;
        Some(spend.total_cents())
    }

    async fn dashboard_post<T: serde::de::DeserializeOwned>(
        &self,
        cookie_header: &str,
        method: &str,
        body: serde_json::Value,
    ) -> Result<T, ProviderError> {
        let response = self
            .client
            .post(format!("{BASE_URL}/api/dashboard/{method}"))
            .header("Cookie", cookie_header)
            .header("Origin", BASE_URL)
            .header("Accept", "application/json")
            .json(&body)
            .timeout(DASHBOARD_TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "Cursor {method} returned status {status}"
            )));
        }
        response
            .json()
            .await
            .map_err(|error| ProviderError::Parse(error.to_string()))
    }

    pub(super) fn get_cookie_header(&self) -> Result<String, ProviderError> {
        browser_cookie_header(&COOKIE_DOMAINS)
    }

    async fn fetch_usage_summary(
        &self,
        cookie_header: &str,
    ) -> Result<UsageSummary, ProviderError> {
        let url = format!("{}/api/usage-summary", BASE_URL);

        let response = self
            .client
            .get(&url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        if response.status() == 401 || response.status() == 403 {
            return Err(ProviderError::AuthRequired);
        }

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Cursor API returned {}",
                response.status()
            )));
        }

        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;
        serde_json::from_str::<UsageSummary>(&text).map_err(|e| {
            tracing::warn!(
                "Cursor usage-summary parse error: {e}; response length: {} bytes",
                text.len()
            );
            ProviderError::Parse(e.to_string())
        })
    }

    async fn fetch_user_info(&self, cookie_header: &str) -> Result<UserInfo, ProviderError> {
        let url = format!("{}/api/auth/me", BASE_URL);

        let response = self
            .client
            .get(&url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(
                "Failed to fetch user info".to_string(),
            ));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))
    }

    fn build_result(
        &self,
        summary: UsageSummary,
        user_info: Option<UserInfo>,
    ) -> Result<CursorUsageResult, ProviderError> {
        let billing_end = summary
            .billing_cycle_end
            .as_ref()
            .and_then(|s| parse_iso_date(s));
        let window_minutes = monthly_window_minutes(
            summary.billing_cycle_start.as_deref(),
            summary.billing_cycle_end.as_deref(),
        );

        let mut extras = Vec::new();
        let mut inactives = Vec::new();
        let mut secondary = None;
        let mut cost_snapshot = None;
        let mut monthly_percent = None;

        // An empty `individualUsage: {}` object is not a reading. Fall through
        // to teamUsage instead of fabricating a 0% monthly meter.
        if let Some(individual) = summary
            .individual_usage
            .as_ref()
            .filter(|usage| individual_has_usage(usage))
        {
            if let Some(plan) = &individual.plan {
                monthly_percent = plan_monthly_percent(plan);

                if let Some(auto) = plan.auto_percent_used {
                    secondary = Some(RateWindow::with_details(
                        auto,
                        window_minutes,
                        billing_end,
                        None,
                    ));
                }

                if let Some(api) = plan.api_percent_used {
                    extras.push(NamedRateWindow::new(
                        "cursor-api",
                        "API",
                        RateWindow::with_details(api, window_minutes, billing_end, None),
                    ));
                }

                // When Cursor reports the modern percent-lane payload, omitted Auto/API
                // lanes are explicit absences — not fabricated 0% meters.
                if uses_percent_lanes(plan) {
                    if plan.auto_percent_used.is_none() {
                        inactives.push(InactiveRateWindow::new(
                            "cursor-auto",
                            "Auto",
                            NOT_ENFORCED,
                        ));
                    }
                    if plan.api_percent_used.is_none() {
                        inactives.push(InactiveRateWindow::new("cursor-api", "API", NOT_ENFORCED));
                    }
                }
            } else if let Some(overall) = &individual.overall {
                // Overall is a single reported pool — keep it as monthly + cost only.
                monthly_percent = Self::usage_percent(overall);
                // Overall fills the cost slot as included units. On-demand still
                // surfaces as its own meter below; it must not replace this total.
                cost_snapshot = Self::on_demand_cost(Some(overall), billing_end, INCLUDED_PERIOD);
            }

            // On-demand is a sibling of plan/overall on IndividualUsage. Read it
            // once for every individual shape — including payloads that only
            // carry onDemand and omit both plan and overall.
            let on_demand = individual.on_demand.as_ref().or_else(|| {
                summary
                    .team_usage
                    .as_ref()
                    .and_then(|t| t.on_demand.as_ref())
            });
            let (metered, notice) = on_demand_windows(on_demand, window_minutes, billing_end);
            extras.extend(metered);
            inactives.extend(notice);

            // Cost ownership by shape:
            // - plan: on-demand (real money) outranks plan internal units
            // - overall: already set above; leave the monthly pool cost alone
            // - on-demand only: on-demand is the only billed signal
            if individual.plan.is_some() {
                cost_snapshot = Self::on_demand_cost(on_demand, billing_end, ON_DEMAND_PERIOD)
                    .or_else(|| {
                        individual
                            .plan
                            .as_ref()
                            .and_then(|plan| plan_cost_snapshot(plan, billing_end))
                    });
            } else if individual.overall.is_none() {
                cost_snapshot = Self::on_demand_cost(on_demand, billing_end, ON_DEMAND_PERIOD);
            }
        } else if let Some(team) = &summary.team_usage {
            if let Some(pooled) = &team.pooled {
                monthly_percent = Self::usage_percent(pooled);
            }
            let (metered, notice) =
                on_demand_windows(team.on_demand.as_ref(), window_minutes, billing_end);
            extras.extend(metered);
            inactives.extend(notice);
            // Same rule as the individual branch: on-demand is the billed lane,
            // so it takes the cost slot ahead of the pooled plan allowance.
            cost_snapshot =
                Self::on_demand_cost(team.on_demand.as_ref(), billing_end, ON_DEMAND_PERIOD);
            if cost_snapshot.is_none() {
                cost_snapshot = team.pooled.as_ref().and_then(|pooled| {
                    Self::on_demand_cost(Some(pooled), billing_end, INCLUDED_PERIOD)
                });
            }
        }

        let has_monthly_meter = monthly_percent.is_some();
        if !has_monthly_meter {
            // UsageSnapshot always needs a primary window, but 0% is not a
            // reading. Mark Monthly unavailable (or not enforced when Cursor
            // says the plan is unlimited) so glance surfaces can tell.
            if summary.is_unlimited == Some(true) {
                inactives.push(InactiveRateWindow::new(
                    "cursor-monthly",
                    "Monthly",
                    NOT_ENFORCED,
                ));
            } else {
                // Title must stay Plan so EnforcementTracker shares the
                // primary identity. "Monthly" would stick as a false
                // unavailable row after a later plan reading appears.
                inactives.push(InactiveRateWindow::unavailable(
                    "cursor-plan",
                    "Plan",
                    NO_USAGE_REPORTED,
                ));
            }
        }

        let primary = RateWindow::with_details(
            monthly_percent.unwrap_or(0.0),
            window_minutes,
            billing_end,
            None,
        );

        let mut usage = UsageSnapshot::new(primary);
        if let Some(sec) = secondary {
            usage = usage.with_secondary(sec);
        }
        usage.extra_rate_windows = extras;
        usage.inactive_rate_windows = inactives;

        if let Some(email) = user_info.as_ref().and_then(|u| u.email.clone()) {
            usage = usage.with_email(email);
        }
        if let Some(plan_type) = membership_label(summary.membership_type.as_deref()) {
            usage = usage.with_login_method(plan_type);
        }

        Ok((usage, cost_snapshot))
    }

    fn on_demand_cost(
        on_demand: Option<&OnDemandUsage>,
        billing_end: Option<DateTime<Utc>>,
        period: &str,
    ) -> Option<CostSnapshot> {
        let usage = on_demand?;
        if usage.enabled == Some(false) {
            return None;
        }

        let used_cents = usage.used.unwrap_or(0) as f64;
        let limit_cents = usage
            .limit
            .or_else(|| {
                usage
                    .remaining
                    .map(|remaining| remaining + usage.used.unwrap_or(0))
            })
            .unwrap_or(0) as f64;

        if used_cents <= 0.0 && limit_cents <= 0.0 {
            return None;
        }

        let mut cost = CostSnapshot::new(used_cents / 100.0, "USD", period);
        if limit_cents > 0.0 {
            cost = cost.with_limit(limit_cents / 100.0);
        }
        if let Some(reset) = billing_end {
            cost = cost.with_resets_at(reset);
        }
        Some(cost)
    }

    fn usage_percent(usage: &OnDemandUsage) -> Option<f64> {
        let used = usage.used.unwrap_or(0) as f64;
        let limit = usage
            .limit
            .or_else(|| {
                usage
                    .remaining
                    .map(|remaining| remaining + usage.used.unwrap_or(0))
            })
            .unwrap_or(0) as f64;
        (limit > 0.0).then_some(used / limit * 100.0)
    }
}

impl Default for CursorApi {
    fn default() -> Self {
        Self::new()
    }
}

// --- API Response Types ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSummary {
    billing_cycle_start: Option<String>,
    billing_cycle_end: Option<String>,
    membership_type: Option<String>,
    #[allow(dead_code)]
    limit_type: Option<String>,
    is_unlimited: Option<bool>,
    individual_usage: Option<IndividualUsage>,
    team_usage: Option<TeamUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndividualUsage {
    plan: Option<PlanUsage>,
    on_demand: Option<OnDemandUsage>,
    overall: Option<OnDemandUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanUsage {
    #[allow(dead_code)]
    enabled: Option<bool>,
    used: Option<i64>,
    limit: Option<i64>,
    #[allow(dead_code)]
    remaining: Option<i64>,
    breakdown: Option<PlanBreakdown>,
    auto_percent_used: Option<f64>,
    api_percent_used: Option<f64>,
    total_percent_used: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanBreakdown {
    included: Option<i64>,
    bonus: Option<i64>,
    total: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnDemandUsage {
    enabled: Option<bool>,
    used: Option<i64>,
    limit: Option<i64>,
    remaining: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamUsage {
    on_demand: Option<OnDemandUsage>,
    pooled: Option<OnDemandUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    email: Option<String>,
    #[allow(dead_code)]
    email_verified: Option<bool>,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    sub: Option<String>,
    #[allow(dead_code)]
    created_at: Option<String>,
    #[allow(dead_code)]
    updated_at: Option<String>,
    #[allow(dead_code)]
    picture: Option<String>,
}

/// Grok Bot's weekly meter. On The X Subscription it is the whole account's
/// shared pool; on a Cursor plan it is a separate allowance beside that plan.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SandUsageStatus {
    next_reset_timestamp_utc: Option<String>,
    usage_percent: Option<f64>,
    uses_pooled_enterprise_allowance: bool,
    is_unified: bool,
    cursor_plan_name: Option<String>,
    billing_brand: Option<String>,
}

impl SandUsageStatus {
    /// Pooled enterprise seats have no meter of their own, so Grok Bot itself
    /// shows no percentage for them.
    fn reading(&self) -> Option<f64> {
        if self.uses_pooled_enterprise_allowance {
            return None;
        }
        self.usage_percent
            .filter(|percent| percent.is_finite() && *percent >= 0.0)
    }

    fn weekly_window(&self, percent: f64) -> RateWindow {
        let resets_at = self
            .next_reset_timestamp_utc
            .as_deref()
            .and_then(parse_iso_date);
        RateWindow::with_details(percent, Some(WEEKLY_MINUTES), resets_at, None)
    }

    fn unified_plan_label(&self) -> Option<String> {
        let plan = self.cursor_plan_name.as_deref()?.trim();
        if plan.is_empty() {
            return None;
        }
        Some(match self.billing_brand.as_deref() {
            Some("SAND_BILLING_BRAND_GROK") => format!("X {plan}"),
            _ => format!("Cursor {plan}"),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CurrentPeriodUsage {
    plan_usage: Option<PeriodPlanUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PeriodPlanUsage {
    auto_percent_used: Option<f64>,
    api_percent_used: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonthlyBillingCycle {
    start_date_epoch_millis: String,
    end_date_epoch_millis: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DailySpendByCategory {
    daily_spend: Vec<DailySpend>,
}

impl DailySpendByCategory {
    fn total_cents(&self) -> i64 {
        self.daily_spend.iter().map(|day| day.spend_cents).sum()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DailySpend {
    spend_cents: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct UserPrepaidBilling {
    spent_this_month_cents: Option<String>,
}

impl UserPrepaidBilling {
    fn spent_cents(&self) -> Option<i64> {
        self.spent_this_month_cents.as_deref()?.parse().ok()
    }
}

#[derive(Debug)]
struct GrokBotReading {
    status: SandUsageStatus,
    period: Option<CurrentPeriodUsage>,
    credits_spent_cents: Option<i64>,
    on_demand_cents: Option<i64>,
}

fn apply_grok_bot(
    usage: &mut UsageSnapshot,
    cost: &mut Option<CostSnapshot>,
    reading: &GrokBotReading,
) {
    let Some(percent) = reading.status.reading() else {
        return;
    };
    if reading.status.is_unified {
        apply_unified_plan(usage, cost, reading, percent);
    } else {
        apply_grok_bot_allowance(usage, reading, percent);
    }
}

/// On The X Subscription, Cursor, Grok Bot and the Grok apps draw on one weekly
/// pool. `usage-summary` still reports a calendar month at 0% for these accounts,
/// so the weekly reading replaces its plan lanes rather than sitting beside them.
fn apply_unified_plan(
    usage: &mut UsageSnapshot,
    cost: &mut Option<CostSnapshot>,
    reading: &GrokBotReading,
    percent: f64,
) {
    let status = &reading.status;
    let plan = reading
        .period
        .as_ref()
        .and_then(|period| period.plan_usage.as_ref());

    usage.primary = status.weekly_window(percent);
    usage.secondary = plan
        .and_then(|plan| plan.auto_percent_used)
        .map(|auto| status.weekly_window(auto));
    usage
        .extra_rate_windows
        .retain(|window| window.id != API_ID);
    if let Some(api) = plan.and_then(|plan| plan.api_percent_used) {
        usage.extra_rate_windows.insert(
            0,
            NamedRateWindow::new(API_ID, API_TITLE, status.weekly_window(api)),
        );
    }
    usage.inactive_rate_windows.retain(|window| {
        !matches!(
            window.id.as_str(),
            "cursor-plan" | "cursor-monthly" | "cursor-auto" | API_ID
        )
    });
    if let Some(label) = status.unified_plan_label() {
        usage.login_method = Some(label);
    }

    // The monthly included figure describes the same stale lanes. Billed
    // on-demand from usage-summary is still real money, so it stays.
    if cost
        .as_ref()
        .is_some_and(|cost| cost.period == INCLUDED_PERIOD)
    {
        *cost = None;
    }
    if let Some(cents) = reading.credits_spent_cents.filter(|cents| *cents > 0) {
        let mut credits = CostSnapshot::new(cents as f64 / 100.0, "USD", CREDITS_PERIOD);
        if let Some(reset) = usage.primary.resets_at {
            credits = credits.with_resets_at(reset);
        }
        *cost = Some(credits);
    }
}

/// On a Cursor plan or a linked Grok subscription, Grok Bot's weekly allowance
/// is separate from the plan. Untouched allowances stay quiet, since most Cursor
/// users never run a bot and a permanent 0% row would only be noise.
fn apply_grok_bot_allowance(usage: &mut UsageSnapshot, reading: &GrokBotReading, percent: f64) {
    let spent = reading.on_demand_cents.filter(|cents| *cents > 0);
    if percent <= 0.0 && spent.is_none() {
        return;
    }
    let mut window = NamedRateWindow::new(
        GROK_BOT_ID,
        GROK_BOT_TITLE,
        reading.status.weekly_window(percent),
    );
    if let Some(cents) = spent {
        window = window.with_amount(WindowAmount::new(cents as f64 / 100.0, "USD"));
    }
    usage.extra_rate_windows.push(window);
}

// --- Helper functions ---

fn plan_monthly_percent(plan: &PlanUsage) -> Option<f64> {
    if let Some(percent) = plan.total_percent_used {
        return Some(percent);
    }

    let used_cents = plan.used.unwrap_or(0) as f64;
    let limit_cents = plan
        .limit
        .or_else(|| plan.breakdown.as_ref().and_then(|b| b.total))
        .unwrap_or(0) as f64;
    (limit_cents > 0.0).then_some((used_cents / limit_cents) * 100.0)
}

fn uses_percent_lanes(plan: &PlanUsage) -> bool {
    plan.total_percent_used.is_some()
        || plan.auto_percent_used.is_some()
        || plan.api_percent_used.is_some()
}

fn individual_has_usage(individual: &IndividualUsage) -> bool {
    if individual.overall.is_some() || individual.on_demand.is_some() {
        return true;
    }
    individual.plan.as_ref().is_some_and(|plan| {
        plan_monthly_percent(plan).is_some()
            || plan.auto_percent_used.is_some()
            || plan.api_percent_used.is_some()
            || plan_cost_snapshot(plan, None).is_some()
    })
}

/// Cursor reports on-demand two ways: as a capped pool that can be metered, or
/// as uncapped spend with no limit at all. Uncapped overdraft is still real
/// money, so surface it as an explicit non-metering window instead of dropping
/// it on the floor for want of a denominator.
fn on_demand_windows(
    on_demand: Option<&OnDemandUsage>,
    window_minutes: Option<u32>,
    billing_end: Option<DateTime<Utc>>,
) -> (Option<NamedRateWindow>, Option<InactiveRateWindow>) {
    let Some(usage) = on_demand else {
        return (None, None);
    };
    if usage.enabled == Some(false) {
        return (None, None);
    }

    if let Some(percent) = CursorApi::usage_percent(usage) {
        let mut window = NamedRateWindow::new(
            ON_DEMAND_ID,
            ON_DEMAND_TITLE,
            RateWindow::with_details(percent, window_minutes, billing_end, None),
        );
        // The percentage drives the bar, but overdraft is money — show it.
        if let Some(amount) = on_demand_amount(usage) {
            window = window.with_amount(amount);
        }
        return (Some(window), None);
    }

    // No limit and no remaining: nothing to meter against, but the spend itself
    // is worth showing once the plan is exhausted.
    let used_cents = usage.used.unwrap_or(0);
    if used_cents <= 0 {
        return (None, None);
    }
    let spent = WindowAmount::new(used_cents as f64 / 100.0, "USD").format_used();
    let notice = InactiveRateWindow::new(
        ON_DEMAND_ID,
        ON_DEMAND_TITLE,
        format!("{spent} spent, no spend limit set"),
    );
    (None, Some(notice))
}

/// The plan lane expressed in currency.
///
/// Note this is *not* money owed. Cursor prices included usage at internal
/// rates, and an exported usage report shows every such event as "Included" or
/// "Free" with no charge. Only the on-demand lane is real billing, which is why
/// it takes the cost slot ahead of this.
fn plan_cost_snapshot(
    plan: &PlanUsage,
    billing_end: Option<DateTime<Utc>>,
) -> Option<CostSnapshot> {
    let used_cents = plan.used? as f64;
    let limit_cents = plan
        .limit
        .or_else(|| plan.breakdown.as_ref().and_then(|b| b.total))
        .unwrap_or(0) as f64;

    if used_cents <= 0.0 && limit_cents <= 0.0 {
        return None;
    }

    let mut cost = CostSnapshot::new(used_cents / 100.0, "USD", INCLUDED_PERIOD);
    if limit_cents > 0.0 {
        cost = cost.with_limit(limit_cents / 100.0);
    }
    if let Some(reset) = billing_end {
        cost = cost.with_resets_at(reset);
    }
    Some(cost)
}

/// Dollars behind an on-demand lane, for display beside its meter.
fn on_demand_amount(usage: &OnDemandUsage) -> Option<WindowAmount> {
    // Treat an omitted `used` as zero, the way `usage_percent` does, so a
    // reported cap still renders instead of the amount vanishing entirely.
    let used = usage.used.unwrap_or(0) as f64;
    let mut amount = WindowAmount::new(used / 100.0, "USD");
    if let Some(limit) = usage
        .limit
        .or_else(|| usage.remaining.map(|r| r + usage.used.unwrap_or(0)))
        .filter(|&l| l > 0)
    {
        amount = amount.with_limit(limit as f64 / 100.0);
    }
    Some(amount)
}

fn membership_label(membership: Option<&str>) -> Option<String> {
    membership.map(|t| match t.to_lowercase().as_str() {
        "enterprise" => "Cursor Enterprise".to_string(),
        "pro" => "Cursor Pro".to_string(),
        "hobby" => "Cursor Hobby".to_string(),
        "team" => "Cursor Team".to_string(),
        other => format!("Cursor {}", capitalize(other)),
    })
}

fn monthly_window_minutes(start: Option<&str>, end: Option<&str>) -> Option<u32> {
    let start = parse_iso_date(start?)?;
    let end = parse_iso_date(end?)?;
    let minutes = (end - start).num_minutes();
    (minutes > 0).then_some(minutes as u32)
}

fn parse_iso_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.with_timezone(&Utc));
    }

    None
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> CursorApi {
        CursorApi::new()
    }

    fn parse_summary(json: &str) -> UsageSummary {
        serde_json::from_str(json).expect("fixture should parse")
    }

    fn extra<'a>(usage: &'a UsageSnapshot, id: &str) -> &'a NamedRateWindow {
        usage
            .extra_rate_windows
            .iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| panic!("expected extra window {id}"))
    }

    fn inactive<'a>(usage: &'a UsageSnapshot, id: &str) -> &'a InactiveRateWindow {
        usage
            .inactive_rate_windows
            .iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| panic!("expected inactive window {id}"))
    }

    #[test]
    fn test_cursor_build_result_with_lanes() {
        let json = r#"{
            "billingCycleStart": "2026-03-01T00:00:00Z",
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 1500,
                    "limit": 5000,
                    "totalPercentUsed": 30.0,
                    "autoPercentUsed": 20.0,
                    "apiPercentUsed": 10.0
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 30.0).abs() < 0.01);
        assert!(usage.primary.resets_at.is_some());
        assert_eq!(usage.primary.window_minutes, Some(31 * 24 * 60));

        let sec = usage.secondary.as_ref().expect("Auto should be present");
        assert!((sec.used_percent - 20.0).abs() < 0.01);
        assert!(sec.resets_at.is_some());

        let api_window = extra(&usage, "cursor-api");
        assert_eq!(api_window.title, "API");
        assert!((api_window.window.used_percent - 10.0).abs() < 0.01);
        assert!(api_window.window.resets_at.is_some());

        assert!(usage.inactive_rate_windows.is_empty());
        assert!(cost.is_some());
        assert_eq!(usage.login_method.as_deref(), Some("Cursor Pro"));
    }

    #[test]
    fn test_cursor_build_result_prefers_api_percent_fields() {
        let json = r#"{
            "membershipType": "pro",
            "autoModelSelectedDisplayMessage": "You've used 13% of your included total usage",
            "individualUsage": {
                "plan": {
                    "used": 2000,
                    "limit": 2000,
                    "breakdown": {
                        "included": 2000,
                        "bonus": 580,
                        "total": 2580
                    },
                    "autoPercentUsed": 17.2,
                    "apiPercentUsed": 0,
                    "totalPercentUsed": 13.230769230769232
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 13.230769230769232).abs() < 0.01);
        assert!((usage.secondary.as_ref().unwrap().used_percent - 17.2).abs() < 0.01);
        assert!((extra(&usage, "cursor-api").window.used_percent - 0.0).abs() < 0.01);

        // No Promotional lane: `breakdown.bonus` is bonus *consumed*, and the
        // grant it would be metered against is nowhere in the payload.
        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "cursor-promotional")
        );

        let cost = cost.expect("plan usage should still produce cost snapshot");
        assert!((cost.used - 20.0).abs() < 0.01);
        assert_eq!(cost.limit, Some(20.0));
        assert_eq!(usage.login_method.as_deref(), Some("Cursor Pro"));
    }

    #[test]
    fn test_cursor_build_result_cents_only() {
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 2500,
                    "limit": 5000
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 50.0).abs() < 0.01);
        assert!(usage.secondary.is_none(), "no autoPercentUsed in payload");
        assert!(usage.extra_rate_windows.is_empty());
        assert!(
            usage.inactive_rate_windows.is_empty(),
            "cents-only payloads are not the percent-lane format"
        );
        assert!(cost.is_some());
    }

    #[test]
    fn test_cursor_percent_falls_back_to_the_breakdown_total_limit() {
        // No `totalPercentUsed` and no `plan.limit`: the percentage falls back
        // to metering `used` against `breakdown.total`.
        let json = r#"{
            "individualUsage": {
                "plan": {
                    "used": 1250,
                    "breakdown": {"included": 5000, "bonus": 0, "total": 5000}
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, _) = api().build_result(summary, None).unwrap();
        assert!((usage.primary.used_percent - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_cursor_build_result_missing_plan() {
        let json = r#"{
            "membershipType": "hobby",
            "individualUsage": {}
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent).abs() < 0.01);
        assert!(usage.secondary.is_none());
        assert!(usage.extra_rate_windows.is_empty());
        let monthly = inactive(&usage, "cursor-plan");
        assert_eq!(monthly.title, "Plan");
        assert_eq!(monthly.description, NO_USAGE_REPORTED);
        assert_eq!(monthly.state, EnforcementState::Unavailable);
        assert!(cost.is_none());
    }

    #[test]
    fn test_cursor_on_demand_only_individual_usage() {
        // individualUsage may carry only onDemand (plan and overall both null).
        // That shape used to fall through both arms and drop the overdraft lane.
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "onDemand": {
                    "enabled": true,
                    "used": 350,
                    "limit": 1000
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        let on_demand = extra(&usage, "cursor-on-demand");
        assert_eq!(on_demand.title, "On-demand");
        assert!((on_demand.window.used_percent - 35.0).abs() < 0.01);

        let cost = cost.expect("on-demand-only payload still reports spend");
        assert!((cost.used - 3.5).abs() < 0.01);
        assert_eq!(cost.limit, Some(10.0));
        assert_eq!(cost.period, "On-demand");
        // No plan/overall reading: Monthly is missing, not 0%.
        let monthly = inactive(&usage, "cursor-plan");
        assert_eq!(monthly.title, "Plan");
        assert_eq!(monthly.state, EnforcementState::Unavailable);
    }

    #[test]
    fn test_cursor_on_demand_as_named_extra_and_cost() {
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 800,
                    "limit": 5000,
                    "totalPercentUsed": 16.0
                },
                "onDemand": {
                    "enabled": true,
                    "used": 350,
                    "limit": 1000
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 16.0).abs() < 0.01);

        let on_demand = extra(&usage, "cursor-on-demand");
        assert_eq!(on_demand.title, "On-demand");
        assert!((on_demand.window.used_percent - 35.0).abs() < 0.01);
        assert!(on_demand.window.resets_at.is_some());

        // The overdraft meter carries its own dollars, so it stays visible even
        // though the single cost slot belongs to plan spend.
        let amount = on_demand.amount.as_ref().expect("on-demand carries money");
        assert!((amount.used - 3.50).abs() < 0.01);
        assert_eq!(amount.limit, Some(10.0));
        assert_eq!(amount.format_used(), "$3.50");

        // Lane format with only total â†’ Auto/API inactive
        assert_eq!(usage.inactive_rate_windows.len(), 2);

        // On-demand is the only real-money lane, so it owns the cost slot.
        let cost = cost.expect("on-demand cost");
        assert!((cost.used - 3.5).abs() < 0.01);
        assert_eq!(cost.limit, Some(10.0));
        assert_eq!(cost.period, "On-demand");
    }

    #[test]
    fn test_cursor_unlimited_without_monthly_meter() {
        let json = r#"{
            "membershipType": "hobby",
            "isUnlimited": true,
            "individualUsage": {}
        }"#;

        let summary = parse_summary(json);
        let (usage, _) = api().build_result(summary, None).unwrap();

        let monthly = inactive(&usage, "cursor-monthly");
        assert_eq!(monthly.title, "Monthly");
        assert_eq!(monthly.description, NOT_ENFORCED);
    }

    #[test]
    fn test_cursor_reports_no_promotional_meter() {
        // The old meter computed `plan.used - breakdown.included`, which is
        // structurally zero: the included lane is its own closed set, so it
        // read 0% forever. `breakdown.bonus` is bonus consumed, not a grant,
        // so there is nothing honest to meter it against.
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 2000,
                    "limit": 2000,
                    "remaining": 0,
                    "breakdown": {
                        "included": 2000,
                        "bonus": 23466,
                        "total": 25466
                    },
                    "totalPercentUsed": 73.81449275362318
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, _) = api().build_result(summary, None).unwrap();

        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "cursor-promotional")
        );
        // And no promo badge, which carried a billing-cycle end date while
        // Cursor credits expire on their own schedule.
        assert!(usage.promo_signals.is_empty());
        // The primary meter is unaffected: it reads totalPercentUsed directly.
        assert!((usage.primary.used_percent - 73.81449275362318).abs() < 0.01);
    }

    #[test]
    fn test_cursor_uncapped_on_demand_reports_spend() {
        // On-demand enabled with real spend but no cap: there is no denominator
        // to meter, so the spend surfaces as an explicit non-metering window.
        let json = r#"{
            "billingCycleEnd": "2026-09-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 2000,
                    "limit": 2000,
                    "totalPercentUsed": 100.0
                },
                "onDemand": {
                    "enabled": true,
                    "used": 1234
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "cursor-on-demand"),
            "no cap means no meter"
        );
        let notice = inactive(&usage, "cursor-on-demand");
        assert_eq!(notice.title, "On-demand");
        assert_eq!(notice.description, "$12.34 spent, no spend limit set");

        let cost = cost.expect("uncapped on-demand still reports spend");
        assert!((cost.used - 12.34).abs() < 0.01);
        assert_eq!(cost.limit, None);
        assert_eq!(cost.period, "On-demand");
    }

    #[test]
    fn test_cursor_on_demand_surfaces_alongside_overall() {
        let json = r#"{
            "individualUsage": {
                "overall": {"used": 2500, "limit": 2500},
                "onDemand": {"enabled": true, "used": 900, "limit": 5000}
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 100.0).abs() < 0.01);
        let on_demand = extra(&usage, "cursor-on-demand");
        assert!((on_demand.window.used_percent - 18.0).abs() < 0.01);

        // `overall` remains the reported total pool, labeled as included
        // units rather than billed on-demand spend.
        let cost = cost.expect("overall cost");
        assert!((cost.used - 25.0).abs() < 0.01);
        assert_eq!(cost.period, INCLUDED_PERIOD);
    }

    #[test]
    fn test_cursor_zero_spend_uncapped_on_demand_is_silent() {
        let json = r#"{
            "individualUsage": {
                "plan": {"used": 100, "limit": 1000, "totalPercentUsed": 10.0},
                "onDemand": {"enabled": true, "used": 0}
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, _) = api().build_result(summary, None).unwrap();

        assert!(
            !usage
                .inactive_rate_windows
                .iter()
                .any(|w| w.id == "cursor-on-demand")
        );
        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "cursor-on-demand")
        );
    }

    #[test]
    fn test_cursor_disabled_on_demand_is_ignored() {
        let json = r#"{
            "individualUsage": {
                "plan": {
                    "used": 100,
                    "limit": 1000,
                    "totalPercentUsed": 10.0,
                    "autoPercentUsed": 5.0,
                    "apiPercentUsed": 2.0
                },
                "onDemand": {
                    "enabled": false,
                    "used": 50,
                    "limit": 500
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "cursor-on-demand")
        );
        // Falls back to plan cents when on-demand is disabled
        let cost = cost.expect("plan cost fallback");
        assert!((cost.used - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cursor_individual_overall_fallback() {
        let summary =
            parse_summary(r#"{"individualUsage":{"overall":{"used":2500,"limit":10000}}}"#);
        let (usage, cost) = api().build_result(summary, None).unwrap();
        assert!((usage.primary.used_percent - 25.0).abs() < 0.01);
        assert_eq!(cost.unwrap().limit, Some(100.0));
        assert!(usage.extra_rate_windows.is_empty());
    }

    #[test]
    fn test_cursor_on_demand_cap_renders_without_a_used_field() {
        // Cursor can report a cap with no `used`. That is zero spend against a
        // real limit, not an absent amount.
        let json = r#"{
            "individualUsage": {
                "plan": {"used": 100, "limit": 1000, "totalPercentUsed": 10.0},
                "onDemand": {"enabled": true, "limit": 2000}
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, _) = api().build_result(summary, None).unwrap();

        let amount = extra(&usage, "cursor-on-demand")
            .amount
            .as_ref()
            .expect("a cap is still an amount");
        assert_eq!(amount.used, 0.0);
        assert_eq!(amount.limit, Some(20.0));
    }

    #[test]
    fn test_cursor_team_on_demand_outranks_pooled_for_cost() {
        // On-demand is the billed lane for teams too, so it must not be
        // shadowed by the pooled plan allowance.
        let json = r#"{
            "teamUsage": {
                "pooled": {"used": 5000, "limit": 10000},
                "onDemand": {"enabled": true, "used": 750, "limit": 2000}
            }
        }"#;

        let summary = parse_summary(json);
        let (usage, cost) = api().build_result(summary, None).unwrap();

        assert!((usage.primary.used_percent - 50.0).abs() < 0.01);
        let cost = cost.expect("on-demand cost");
        assert!((cost.used - 7.5).abs() < 0.01);
        assert_eq!(cost.period, "On-demand");
    }

    #[test]
    fn test_cursor_team_pooled_fallback() {
        let summary = parse_summary(r#"{"teamUsage":{"pooled":{"used":5000,"limit":10000}}}"#);
        let (usage, cost) = api().build_result(summary, None).unwrap();
        assert!((usage.primary.used_percent - 50.0).abs() < 0.01);
        let cost = cost.expect("pooled included units");
        assert_eq!(cost.used, 50.0);
        assert_eq!(cost.period, INCLUDED_PERIOD);
        assert!(usage.extra_rate_windows.is_empty());
    }

    #[test]
    fn cursor_usage_fixtures_cover_normal_partial_duplicate_and_malformed() {
        let (normal, normal_cost) = api()
            .build_result(
                parse_summary(include_str!("../fixtures/cursor/normal.json")),
                None,
            )
            .expect("normal fixture");
        assert!((normal.primary.used_percent - 16.0).abs() < 0.01);
        assert!((normal.secondary.as_ref().unwrap().used_percent - 20.0).abs() < 0.01);
        assert!((extra(&normal, "cursor-api").window.used_percent - 10.0).abs() < 0.01);
        let billed = extra(&normal, ON_DEMAND_ID);
        assert_eq!(billed.title, ON_DEMAND_TITLE);
        assert!((billed.window.used_percent - 35.0).abs() < 0.01);
        let cost = normal_cost.expect("on-demand owns the cost slot");
        assert_eq!(cost.period, ON_DEMAND_PERIOD);
        assert!((cost.used - 3.5).abs() < 0.01);
        assert!(
            !normal
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "cursor-monthly")
        );

        let (partial, partial_cost) = api()
            .build_result(
                parse_summary(include_str!("../fixtures/cursor/partial.json")),
                None,
            )
            .expect("partial fixture");
        assert!(partial.secondary.is_none());
        assert!((extra(&partial, ON_DEMAND_ID).window.used_percent - 35.0).abs() < 0.01);
        assert_eq!(
            partial_cost.expect("partial still bills on-demand").period,
            ON_DEMAND_PERIOD
        );
        let monthly = inactive(&partial, "cursor-plan");
        assert_eq!(monthly.title, "Plan");
        assert_eq!(monthly.state, EnforcementState::Unavailable);
        assert_eq!(monthly.description, NO_USAGE_REPORTED);

        let (duplicate, duplicate_cost) = api()
            .build_result(
                parse_summary(include_str!("../fixtures/cursor/duplicate.json")),
                None,
            )
            .expect("duplicate fixture");
        // Individual wins when both individual and team payloads are present.
        assert!((duplicate.primary.used_percent - 16.0).abs() < 0.01);
        assert!((extra(&duplicate, ON_DEMAND_ID).window.used_percent - 35.0).abs() < 0.01);
        let duplicate_cost = duplicate_cost.expect("individual on-demand");
        assert_eq!(duplicate_cost.period, ON_DEMAND_PERIOD);
        assert!(
            (duplicate_cost.used - 3.5).abs() < 0.01,
            "team on-demand $7.50 must not replace or add to individual $3.50"
        );

        let (malformed, malformed_cost) = api()
            .build_result(
                parse_summary(include_str!("../fixtures/cursor/malformed.json")),
                None,
            )
            .expect("malformed-but-parseable fixture");
        assert!(malformed.primary.resets_at.is_none());
        assert!(malformed.primary.window_minutes.is_none());
        assert!((malformed.primary.used_percent - 12.5).abs() < 0.01);
        let amount = extra(&malformed, ON_DEMAND_ID)
            .amount
            .as_ref()
            .expect("a reported cap is still an amount");
        assert_eq!(amount.used, 0.0);
        assert_eq!(amount.limit, Some(20.0));
        assert_eq!(
            malformed_cost.expect("cap-only on-demand").period,
            ON_DEMAND_PERIOD
        );
    }

    #[test]
    fn empty_individual_usage_falls_through_to_team_instead_of_zero() {
        let (usage, cost) = api()
            .build_result(
                parse_summary(include_str!(
                    "../fixtures/cursor/empty-individual-with-team.json"
                )),
                None,
            )
            .expect("empty individual + team");
        assert!((usage.primary.used_percent - 50.0).abs() < 0.01);
        let cost = cost.expect("team pooled included units");
        assert_eq!(cost.period, INCLUDED_PERIOD);
        assert!((cost.used - 50.0).abs() < 0.01);
        assert!(
            !usage
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "cursor-monthly" || window.id == "cursor-plan")
        );
    }

    #[test]
    fn empty_plan_object_falls_through_to_team() {
        let (usage, cost) = api()
            .build_result(
                parse_summary(include_str!("../fixtures/cursor/empty-plan-with-team.json")),
                None,
            )
            .expect("empty plan + team");
        assert!((usage.primary.used_percent - 50.0).abs() < 0.01);
        let cost = cost.expect("team pooled included units");
        assert_eq!(cost.period, INCLUDED_PERIOD);
        assert!(
            !usage
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "cursor-plan")
        );
    }

    #[test]
    fn unparseable_cursor_summary_fails_closed() {
        let err = serde_json::from_str::<UsageSummary>(include_str!(
            "../fixtures/cursor/unparseable.json"
        ))
        .expect_err("wrong-typed percent must not become 0%");
        assert!(err.to_string().contains("invalid type"));
    }

    const UNIFIED_STATUS: &str = r#"{
        "currentPeriodStart": "2026-10-29T01:33:58.241Z",
        "nextResetTimestampUtc": "2026-11-05T01:33:58.241Z",
        "usagePercent": 3.25,
        "hasAvailableUsage": true,
        "hasNonZeroIncludedLimit": true,
        "grokPlanLabel": "Grok Bot Plan",
        "cursorPlanName": "Premium",
        "isUnified": true,
        "billingBrand": "SAND_BILLING_BRAND_GROK"
    }"#;
    const UNIFIED_PERIOD: &str = r#"{
        "billingCycleStart": "1793237638241",
        "billingCycleEnd": "1793842438241",
        "planUsage": {"autoPercentUsed": 3.25, "apiPercentUsed": 0, "totalPercentUsed": 3.25},
        "spendLimitUsage": {"limitType": "user"}
    }"#;
    const UNIFIED_SUMMARY: &str = r#"{
        "billingCycleStart": "2026-11-01T00:00:00.000Z",
        "billingCycleEnd": "2026-12-01T00:00:00.000Z",
        "membershipType": "premium",
        "limitType": "user",
        "individualUsage": {
            "plan": {"enabled": true, "used": 0, "limit": 4615, "autoPercentUsed": 0,
                     "apiPercentUsed": 0, "totalPercentUsed": 0},
            "onDemand": {"enabled": false, "used": 0, "limit": null, "remaining": null}
        }
    }"#;
    const LEGACY_STATUS: &str = r#"{
        "nextResetTimestampUtc": "2026-11-05T01:33:58.241Z",
        "usagePercent": 43.0,
        "hasNonZeroIncludedLimit": true,
        "grokPlanLabel": "Grok Bot Plan"
    }"#;
    const LEGACY_SUMMARY: &str = r#"{
        "billingCycleStart": "2026-11-01T00:00:00.000Z",
        "billingCycleEnd": "2026-12-01T00:00:00.000Z",
        "membershipType": "ultra",
        "individualUsage": {
            "plan": {"used": 800, "limit": 5000, "autoPercentUsed": 20.0,
                     "apiPercentUsed": 10.0, "totalPercentUsed": 16.0},
            "onDemand": {"enabled": true, "used": 350, "limit": 1000}
        }
    }"#;

    fn status(json: &str) -> SandUsageStatus {
        serde_json::from_str(json).unwrap()
    }

    fn with_grok_bot(summary: &str, reading: GrokBotReading) -> CursorUsageResult {
        let (mut usage, mut cost) = api().build_result(parse_summary(summary), None).unwrap();
        apply_grok_bot(&mut usage, &mut cost, &reading);
        (usage, cost)
    }

    fn unified_reading(period: Option<&str>, credits_spent_cents: Option<i64>) -> GrokBotReading {
        GrokBotReading {
            status: status(UNIFIED_STATUS),
            period: period.map(|json| serde_json::from_str(json).unwrap()),
            credits_spent_cents,
            on_demand_cents: None,
        }
    }

    fn legacy_reading(json: &str, on_demand_cents: Option<i64>) -> GrokBotReading {
        GrokBotReading {
            status: status(json),
            period: None,
            credits_spent_cents: None,
            on_demand_cents,
        }
    }

    fn has_extra(usage: &UsageSnapshot, id: &str) -> bool {
        usage
            .extra_rate_windows
            .iter()
            .any(|window| window.id == id)
    }

    #[test]
    fn unified_plan_replaces_the_stale_monthly_lanes_with_the_weekly_pool() {
        let (usage, cost) =
            with_grok_bot(UNIFIED_SUMMARY, unified_reading(Some(UNIFIED_PERIOD), None));

        assert_eq!(usage.primary.used_percent, 3.25);
        assert_eq!(usage.primary.window_minutes, Some(WEEKLY_MINUTES));
        assert_eq!(
            usage.primary.resets_at,
            parse_iso_date("2026-11-05T01:33:58.241Z")
        );
        let auto = usage.secondary.as_ref().expect("auto lane");
        assert_eq!(auto.used_percent, 3.25);
        assert_eq!(auto.window_minutes, Some(WEEKLY_MINUTES));
        let api_lane = extra(&usage, API_ID);
        assert_eq!(api_lane.window.used_percent, 0.0);
        assert_eq!(api_lane.window.window_minutes, Some(WEEKLY_MINUTES));
        assert_eq!(
            usage
                .extra_rate_windows
                .iter()
                .filter(|window| window.id == API_ID)
                .count(),
            1
        );
        assert!(
            !has_extra(&usage, GROK_BOT_ID),
            "the pool already includes it"
        );
        assert_eq!(usage.login_method.as_deref(), Some("X Premium"));
        assert!(
            cost.is_none(),
            "the monthly included figure is stale: {cost:?}"
        );
    }

    #[test]
    fn unified_plan_without_period_data_drops_lanes_it_cannot_trust() {
        let (usage, _) = with_grok_bot(LEGACY_SUMMARY, unified_reading(None, None));

        assert_eq!(usage.primary.used_percent, 3.25);
        assert!(usage.secondary.is_none());
        assert!(!has_extra(&usage, API_ID));
        assert!(has_extra(&usage, ON_DEMAND_ID), "billed on-demand stays");
    }

    #[test]
    fn unified_plan_reports_credits_spent_past_the_pool() {
        let (_, cost) = with_grok_bot(
            UNIFIED_SUMMARY,
            unified_reading(Some(UNIFIED_PERIOD), Some(1250)),
        );

        let cost = cost.expect("credits");
        assert_eq!(cost.used, 12.5);
        assert_eq!(cost.period, CREDITS_PERIOD);
    }

    #[test]
    fn unified_plan_clears_a_placeholder_plan_row() {
        let (usage, _) = with_grok_bot(
            r#"{"membershipType": "premium"}"#,
            unified_reading(None, None),
        );

        assert_eq!(usage.primary.used_percent, 3.25);
        assert!(usage.inactive_rate_windows.is_empty());
    }

    #[test]
    fn cursor_plan_keeps_its_lanes_and_gains_a_grok_bot_row() {
        let (baseline, baseline_cost) = api()
            .build_result(parse_summary(LEGACY_SUMMARY), None)
            .unwrap();
        let (usage, cost) =
            with_grok_bot(LEGACY_SUMMARY, legacy_reading(LEGACY_STATUS, Some(1234)));

        assert_eq!(usage.primary.used_percent, baseline.primary.used_percent);
        assert_eq!(
            usage.primary.window_minutes,
            baseline.primary.window_minutes
        );
        assert_eq!(usage.login_method, baseline.login_method);
        assert!(usage.secondary.is_some());
        assert_eq!(
            cost.map(|cost| cost.used),
            baseline_cost.map(|cost| cost.used)
        );

        let row = extra(&usage, GROK_BOT_ID);
        assert_eq!(row.title, GROK_BOT_TITLE);
        assert_eq!(row.window.used_percent, 43.0);
        assert_eq!(row.window.window_minutes, Some(WEEKLY_MINUTES));
        assert_eq!(row.amount.as_ref().map(|amount| amount.used), Some(12.34));
    }

    #[test]
    fn untouched_grok_bot_allowance_stays_quiet() {
        let idle = r#"{"usagePercent": 0, "hasNonZeroIncludedLimit": true}"#;
        let (usage, _) = with_grok_bot(LEGACY_SUMMARY, legacy_reading(idle, Some(0)));
        assert!(!has_extra(&usage, GROK_BOT_ID));

        let (usage, _) = with_grok_bot(LEGACY_SUMMARY, legacy_reading(idle, Some(500)));
        assert_eq!(extra(&usage, GROK_BOT_ID).window.used_percent, 0.0);
    }

    #[test]
    fn pooled_or_missing_grok_bot_readings_change_nothing() {
        let baseline = api()
            .build_result(parse_summary(LEGACY_SUMMARY), None)
            .unwrap()
            .0;
        for json in [
            r#"{"usagePercent": 40, "usesPooledEnterpriseAllowance": true}"#,
            r#"{"hasNonZeroIncludedLimit": false}"#,
            r#"{}"#,
            r#"{"usagePercent": -1}"#,
        ] {
            let (usage, _) = with_grok_bot(LEGACY_SUMMARY, legacy_reading(json, Some(900)));
            assert!(!has_extra(&usage, GROK_BOT_ID), "{json}");
            assert_eq!(usage.primary.used_percent, baseline.primary.used_percent);
        }
    }

    #[test]
    fn grok_bot_spend_sums_every_day_and_tolerates_zero_days() {
        let spend: DailySpendByCategory = serde_json::from_str(
            r#"{"dailySpend": [
                {"day": "1", "category": "grok-bot-default", "spendCents": 700, "totalTokens": "5"},
                {"day": "2", "category": "grok-bot-automation", "totalTokens": "9"},
                {"day": "3", "category": "grok-bot-default", "spendCents": 34}
            ], "categories": ["grok-bot-default"]}"#,
        )
        .unwrap();
        assert_eq!(spend.total_cents(), 734);
        assert_eq!(
            serde_json::from_str::<DailySpendByCategory>("{}")
                .unwrap()
                .total_cents(),
            0
        );
    }

    #[test]
    fn prepaid_cents_arrive_as_a_string() {
        let billing: UserPrepaidBilling =
            serde_json::from_str(r#"{"spentThisMonthCents": "1250"}"#).unwrap();
        assert_eq!(billing.spent_cents(), Some(1250));
        assert_eq!(UserPrepaidBilling::default().spent_cents(), None);
    }
}
