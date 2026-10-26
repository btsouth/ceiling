use serde::Deserialize;

use super::UtilizationScale;
use crate::core::{CostSnapshot, NamedRateWindow, RateWindow, UsageSnapshot, WindowAmount};

/// Marks the required primary slot as a placeholder when `five_hour` or its
/// utilization is absent. Glance surfaces treat this id as unknown, not 0%
/// (SBS-1040 / SBS-876).
pub(super) const CLAUDE_SESSION_WINDOW_ID: &str = "claude-session";
const CLAUDE_SESSION_TITLE: &str = "Session (5h)";
const NO_SESSION_USAGE_REPORTED: &str = "No usage reported";

/// Monthly spend lane for seats metered only by an overage spend limit, such
/// as Claude Enterprise. Kept out of capacity events like Cursor's on-demand
/// lane: it measures money, not subscription capacity.
const CLAUDE_SPEND_LIMIT_WINDOW_ID: &str = "claude-spend-limit";
const CLAUDE_SPEND_LIMIT_TITLE: &str = "Monthly spend";

/// Usage payload shared by Claude's OAuth and web endpoints.
///
/// The two endpoints expose the same windows with different casing and have
/// drifted independently in the past. Normalize the wire shape once so every
/// source renders the same set of windows and extra-usage dollars.
#[derive(Debug)]
pub struct ClaudeUsageResponse {
    pub five_hour: Option<ClaudeUsageWindow>,
    pub seven_day: Option<ClaudeUsageWindow>,
    pub seven_day_opus: Option<ClaudeUsageWindow>,
    pub seven_day_sonnet: Option<ClaudeUsageWindow>,
    pub seven_day_oauth_apps: Option<ClaudeUsageWindow>,
    pub seven_day_design: Option<ClaudeUsageWindow>,
    pub seven_day_promotional: Option<ClaudeUsageWindow>,
    pub seven_day_routines: Option<ClaudeUsageWindow>,
    pub extra_usage: Option<ClaudeExtraUsage>,
    pub(super) limits: Vec<super::scoped_weekly::ScopedWeeklyLimit>,
}

impl<'de> Deserialize<'de> for ClaudeUsageResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut map: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::deserialize(deserializer)?;

        let take_window = |map: &mut std::collections::HashMap<String, serde_json::Value>,
                           keys: &[&str]|
         -> Result<Option<ClaudeUsageWindow>, D::Error> {
            for key in keys {
                if let Some(value) = map.remove(*key) {
                    if value.is_null() {
                        continue;
                    }
                    return serde_json::from_value(value)
                        .map(Some)
                        .map_err(serde::de::Error::custom);
                }
            }
            Ok(None)
        };

        let take_value = |map: &mut std::collections::HashMap<String, serde_json::Value>,
                          keys: &[&str]| {
            keys.iter()
                .find_map(|key| map.remove(*key).filter(|value| !value.is_null()))
        };

        Ok(Self {
            five_hour: take_window(&mut map, &["five_hour", "fiveHour"])?,
            seven_day: take_window(&mut map, &["seven_day", "sevenDay"])?,
            seven_day_opus: take_window(&mut map, &["seven_day_opus", "sevenDayOpus"])?,
            seven_day_sonnet: take_window(&mut map, &["seven_day_sonnet", "sevenDaySonnet"])?,
            seven_day_oauth_apps: take_window(
                &mut map,
                &[
                    "seven_day_oauth_apps",
                    "sevenDayOAuthApps",
                    "seven_day_claude_oauth_apps",
                    "oauth_apps",
                    "oauth",
                ],
            )?,
            seven_day_design: take_window(
                &mut map,
                &[
                    "seven_day_design",
                    "sevenDayDesign",
                    "seven_day_claude_design",
                    "claude_design",
                    "design",
                ],
            )?,
            seven_day_promotional: take_window(
                &mut map,
                &[
                    "omelette_promotional",
                    "omelettePromotional",
                    "omelette",
                    "seven_day_omelette",
                    "sevenDayOmelette",
                ],
            )?,
            seven_day_routines: take_window(
                &mut map,
                &[
                    "seven_day_routines",
                    "sevenDayRoutines",
                    "seven_day_claude_routines",
                    "claude_routines",
                    "routines",
                    "routine",
                    "seven_day_cowork",
                    "sevenDayCowork",
                    "cowork",
                ],
            )?,
            limits: take_value(&mut map, &["limits"])
                .map(serde_json::from_value)
                .transpose()
                .map_err(serde::de::Error::custom)?
                .unwrap_or_default(),
            extra_usage: take_value(&mut map, &["extra_usage", "extraUsage"])
                .map(serde_json::from_value)
                .transpose()
                .map_err(serde::de::Error::custom)?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaudeUsageWindow {
    pub utilization: Option<f64>,
    #[serde(rename = "resets_at", alias = "resetsAt")]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeExtraUsage {
    #[serde(
        rename = "monthly_credit_limit",
        alias = "monthlyCreditLimit",
        alias = "monthly_limit",
        alias = "monthlyLimit"
    )]
    pub monthly_limit: Option<f64>,
    #[serde(rename = "used_credits", alias = "usedCredits")]
    pub used_credits: Option<f64>,
    pub currency: Option<String>,
    #[serde(rename = "is_enabled", alias = "isEnabled")]
    pub is_enabled: Option<bool>,
}

impl ClaudeUsageResponse {
    pub(super) fn utilization_scale(&self) -> UtilizationScale {
        UtilizationScale::detect(
            [
                self.five_hour.as_ref(),
                self.seven_day.as_ref(),
                self.seven_day_opus.as_ref(),
                self.seven_day_sonnet.as_ref(),
                self.seven_day_oauth_apps.as_ref(),
                self.seven_day_design.as_ref(),
                self.seven_day_promotional.as_ref(),
                self.seven_day_routines.as_ref(),
            ]
            .into_iter()
            .flatten()
            .filter_map(|window| window.utilization),
        )
    }

    pub(super) fn build_snapshot<F>(&self, mut convert: F) -> UsageSnapshot
    where
        F: FnMut(&ClaudeUsageWindow, Option<u32>, UtilizationScale) -> Option<RateWindow>,
    {
        let scale = self.utilization_scale();
        let primary = self
            .five_hour
            .as_ref()
            .and_then(|window| convert(window, Some(300), scale));
        // UsageSnapshot still needs a primary slot, but 0% is not a reading.
        // Mark Session unavailable so glance surfaces treat it as unknown.
        let session_unknown = primary.is_none();
        let mut snapshot = UsageSnapshot::new(
            primary.unwrap_or_else(|| RateWindow::with_details(0.0, Some(300), None, None)),
        );
        if session_unknown {
            snapshot = snapshot.with_unavailable_rate_window(
                CLAUDE_SESSION_WINDOW_ID,
                CLAUDE_SESSION_TITLE,
                NO_SESSION_USAGE_REPORTED,
            );
        }

        if let Some(window) = self
            .seven_day
            .as_ref()
            .and_then(|window| convert(window, Some(10080), scale))
        {
            snapshot = snapshot.with_secondary(window);
        }

        let model_specific = self
            .seven_day_opus
            .as_ref()
            .and_then(|window| convert(window, Some(10080), scale))
            .or_else(|| {
                self.seven_day_sonnet
                    .as_ref()
                    .and_then(|window| convert(window, Some(10080), scale))
            });
        if let Some(window) = model_specific {
            snapshot = snapshot.with_model_specific(window);
        }

        for (id, title, window) in [
            (
                "claude-oauth-apps",
                "OAuth apps",
                self.seven_day_oauth_apps.as_ref(),
            ),
            (
                "claude-routines",
                "Daily Routines",
                self.seven_day_routines.as_ref(),
            ),
            ("claude-design", "Design", self.seven_day_design.as_ref()),
            (
                "claude-weekly-promo",
                "Weekly promo",
                self.seven_day_promotional.as_ref(),
            ),
        ] {
            if let Some(window) = window.and_then(|window| convert(window, Some(10080), scale)) {
                snapshot
                    .extra_rate_windows
                    .push(NamedRateWindow::new(id, title, window));
            }
        }

        snapshot
            .extra_rate_windows
            .extend(super::scoped_weekly::scoped_weekly_windows(&self.limits));
        snapshot
    }

    pub(super) fn extra_usage_cost(&self) -> Option<CostSnapshot> {
        self.extra_usage.as_ref()?.cost_snapshot()
    }
}

/// Meter the monthly spend limit when it is the only limit Claude reports.
///
/// Enterprise seats have no five-hour or weekly window, just a monthly spend
/// cap, so without this lane every glance surface reads the Session
/// placeholder and shows "Unavailable" (#513). Accounts that report a session
/// or weekly window keep their existing lanes, where spend only starts once
/// the plan runs out.
pub(super) fn with_spend_limit_lane(
    mut snapshot: UsageSnapshot,
    extra: Option<&ClaudeExtraUsage>,
) -> UsageSnapshot {
    let session_reported = !snapshot
        .inactive_rate_windows
        .iter()
        .any(|window| window.id == CLAUDE_SESSION_WINDOW_ID);
    if session_reported || snapshot.secondary.is_some() {
        return snapshot;
    }
    if let Some(window) = extra.and_then(ClaudeExtraUsage::spend_limit_window) {
        snapshot.extra_rate_windows.push(window);
    }
    snapshot
}

impl ClaudeExtraUsage {
    pub(super) fn cost_snapshot(&self) -> Option<CostSnapshot> {
        let extra = self;
        if !extra.is_enabled.unwrap_or(false) {
            return None;
        }

        let mut cost = CostSnapshot::new(
            extra.used_credits.unwrap_or(0.0) / 100.0,
            extra.currency.clone().unwrap_or_else(|| "USD".to_string()),
            "Monthly",
        );
        if let Some(limit) = extra.monthly_limit {
            cost = cost.with_limit(limit / 100.0);
        }
        Some(cost)
    }

    fn spend_limit_window(&self) -> Option<NamedRateWindow> {
        if !self.is_enabled.unwrap_or(false) {
            return None;
        }
        let limit = self.monthly_limit.filter(|limit| *limit > 0.0)?;
        let used = self.used_credits.unwrap_or(0.0);
        let currency = self.currency.clone().unwrap_or_else(|| "USD".to_string());
        Some(
            NamedRateWindow::new(
                CLAUDE_SPEND_LIMIT_WINDOW_ID,
                CLAUDE_SPEND_LIMIT_TITLE,
                RateWindow::new(used / limit * 100.0),
            )
            .with_amount(WindowAmount::new(used / 100.0, currency).with_limit(limit / 100.0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ClaudeUsageResponse;
    use crate::core::RateWindow;

    fn snapshot(response: &ClaudeUsageResponse) -> crate::core::UsageSnapshot {
        response.build_snapshot(|window, minutes, scale| {
            Some(RateWindow::with_details(
                scale.to_percent(window.utilization?),
                minutes,
                None,
                None,
            ))
        })
    }

    #[test]
    fn oauth_shape_keeps_design_and_extra_usage_dollars() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "fiveHour": {"utilization": 12},
                "sevenDayDesign": {"utilization": 34},
                "extraUsage": {
                    "isEnabled": true,
                    "usedCredits": 1234,
                    "monthlyLimit": 5000,
                    "currency": "USD"
                }
            }"#,
        )
        .expect("OAuth response parses");

        let usage = snapshot(&response);
        let design = usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "claude-design")
            .expect("Design window survives OAuth mapping");
        assert_eq!(design.window.used_percent, 34.0);

        let cost = response.extra_usage_cost().expect("extra usage cost");
        assert_eq!(cost.used, 12.34);
        assert_eq!(cost.limit, Some(50.0));
        assert_eq!(cost.currency_code, "USD");
    }

    #[test]
    fn web_shape_keeps_sonnet_as_the_model_specific_window() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 12},
                "seven_day": {"utilization": 23},
                "seven_day_sonnet": {"utilization": 45}
            }"#,
        )
        .expect("web response parses");

        let usage = snapshot(&response);
        assert_eq!(
            usage
                .model_specific
                .expect("Sonnet model-specific window")
                .used_percent,
            45.0
        );
    }

    fn session_placeholder(usage: &crate::core::UsageSnapshot) -> &crate::core::InactiveRateWindow {
        usage
            .inactive_rate_windows
            .iter()
            .find(|window| window.id == "claude-session")
            .expect("Session must stay unknown, not a fabricated 0%")
    }

    /// SBS-1040: a payload that simply omits `five_hour` used to mint
    /// Session (5h) 0%. That is not a reading — the window is unknown.
    #[test]
    fn missing_five_hour_does_not_invent_a_zero_session() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "seven_day": {"utilization": 23}
            }"#,
        )
        .expect("response parses");

        let usage = snapshot(&response);
        let session = session_placeholder(&usage);
        assert_eq!(session.title, "Session (5h)");
        assert_eq!(session.state, crate::core::EnforcementState::Unavailable);
        assert_eq!(usage.secondary.expect("weekly").used_percent, 23.0);
        assert!(
            usage
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "claude-session"),
            "absent five_hour must not render as a real 0% Session"
        );
    }

    /// SBS-1040: `five_hour` present with a null utilization is the same lie.
    #[test]
    fn null_five_hour_utilization_does_not_invent_a_zero_session() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": null, "resets_at": "2026-08-23T12:00:00Z"},
                "seven_day": {"utilization": 23}
            }"#,
        )
        .expect("response parses");

        let usage = snapshot(&response);
        assert_eq!(
            session_placeholder(&usage).state,
            crate::core::EnforcementState::Unavailable
        );
        assert_eq!(usage.secondary.expect("weekly").used_percent, 23.0);
    }

    /// Other windows with a null utilization are omitted, not minted at 0%.
    #[test]
    fn null_weekly_utilization_is_omitted_not_zero() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 12},
                "seven_day": {"utilization": null}
            }"#,
        )
        .expect("response parses");

        let usage = snapshot(&response);
        assert_eq!(usage.primary.used_percent, 12.0);
        assert!(
            usage.secondary.is_none(),
            "null weekly utilization must stay unknown"
        );
        assert!(
            usage
                .inactive_rate_windows
                .iter()
                .all(|window| window.id != "claude-session")
        );
    }

    /// A reported 0% is a reading. Do not mark that unknown.
    #[test]
    fn a_reported_zero_session_stays_a_reading() {
        let response: ClaudeUsageResponse =
            serde_json::from_str(r#"{"five_hour": {"utilization": 0}}"#).expect("response parses");

        let usage = snapshot(&response);
        assert!((usage.primary.used_percent).abs() < f64::EPSILON);
        assert!(
            usage
                .inactive_rate_windows
                .iter()
                .all(|window| window.id != "claude-session"),
            "a reported 0% session is a reading, not unknown"
        );
    }

    /// #513: Enterprise seats report only a monthly spend limit.
    #[test]
    fn spend_limit_is_metered_when_it_is_the_only_limit() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": null,
                "seven_day": null,
                "extra_usage": {
                    "is_enabled": true,
                    "monthly_limit": 18000,
                    "used_credits": 4500,
                    "currency": "USD"
                }
            }"#,
        )
        .expect("usage response");

        let usage =
            super::with_spend_limit_lane(snapshot(&response), response.extra_usage.as_ref());
        let lane = usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "claude-spend-limit")
            .expect("spend lane");
        assert_eq!(lane.title, "Monthly spend");
        assert_eq!(lane.window.used_percent, 25.0);
        let amount = lane.amount.as_ref().expect("amount");
        assert_eq!(amount.used, 45.0);
        assert_eq!(amount.limit, Some(180.0));
    }

    #[test]
    fn spend_limit_over_the_cap_reads_exhausted() {
        let response: ClaudeUsageResponse = serde_json::from_str(
            r#"{"extra_usage": {"is_enabled": true, "monthly_limit": 18000, "used_credits": 18116}}"#,
        )
        .expect("usage response");

        let usage =
            super::with_spend_limit_lane(snapshot(&response), response.extra_usage.as_ref());
        let lane = usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "claude-spend-limit")
            .expect("spend lane");
        assert_eq!(lane.window.used_percent, 100.0);
    }

    #[test]
    fn spend_limit_stays_off_plans_with_a_session_or_weekly_window() {
        let extra =
            r#""extra_usage": {"is_enabled": true, "monthly_limit": 5000, "used_credits": 100}"#;
        for windows in [
            r#""five_hour": {"utilization": 10}"#,
            r#""seven_day": {"utilization": 10}"#,
        ] {
            let response: ClaudeUsageResponse =
                serde_json::from_str(&format!("{{{windows}, {extra}}}")).expect("usage response");
            let usage =
                super::with_spend_limit_lane(snapshot(&response), response.extra_usage.as_ref());
            assert!(
                usage
                    .extra_rate_windows
                    .iter()
                    .all(|window| window.id != "claude-spend-limit"),
                "{windows}"
            );
        }
    }

    #[test]
    fn spend_limit_needs_an_enabled_positive_limit() {
        for extra in [
            r#"{"is_enabled": false, "monthly_limit": 5000, "used_credits": 100}"#,
            r#"{"is_enabled": true, "monthly_limit": 0, "used_credits": 100}"#,
            r#"{"is_enabled": true, "used_credits": 100}"#,
        ] {
            let response: ClaudeUsageResponse =
                serde_json::from_str(&format!(r#"{{"extra_usage": {extra}}}"#))
                    .expect("usage response");
            let usage =
                super::with_spend_limit_lane(snapshot(&response), response.extra_usage.as_ref());
            assert!(usage.extra_rate_windows.is_empty(), "{extra}");
        }
    }
}
