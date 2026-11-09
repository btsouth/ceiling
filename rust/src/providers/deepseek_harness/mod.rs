mod account;
mod logs;
use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};
use async_trait::async_trait;

pub struct DeepSeekHarnessProvider {
    metadata: ProviderMetadata,
}
impl DeepSeekHarnessProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::DeepSeekHarness,
                display_name: "DeepSeek Harness",
                session_label: "DSH",
                weekly_label: "Recorded usage",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://platform.deepseek.com/usage"),
                status_page_url: Some("https://status.deepseek.com"),
            },
        }
    }
}
impl Default for DeepSeekHarnessProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn count(value: u64) -> String {
    let raw = value.to_string();
    let mut s = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && (raw.len() - i).is_multiple_of(3) {
            s.push(',');
        }
        s.push(c);
    }
    s
}
fn row(
    usage: UsageSnapshot,
    id: impl Into<String>,
    title: impl Into<String>,
    value: impl Into<String>,
) -> UsageSnapshot {
    usage.with_extra_rate_window(
        id,
        title,
        RateWindow::with_details(0.0, None, None, Some(value.into())),
    )
}
fn money(wallet: &account::Wallet) -> String {
    let symbol = if wallet.currency == "CNY" { "¥" } else { "$" };
    format!("{symbol}{:.2}", wallet.balance)
}

#[async_trait]
impl Provider for DeepSeekHarnessProvider {
    fn id(&self) -> ProviderId {
        ProviderId::DeepSeekHarness
    }
    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }
    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Cli]
    }
    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        if !matches!(ctx.source_mode, SourceMode::Auto | SourceMode::Cli) {
            return Err(ProviderError::UnsupportedSource(ctx.source_mode));
        }
        let home = ctx
            .account_config_dir
            .clone()
            .or_else(|| std::env::var_os("DSH_HOME").map(std::path::PathBuf::from))
            .or_else(|| dirs::home_dir().map(|p| p.join(".dsh")))
            .ok_or_else(|| {
                ProviderError::NotInstalled("Could not find the DSH home folder".into())
            })?;
        if !home.exists() {
            return Err(ProviderError::NotInstalled(
                "DeepSeek Harness not found. Run it and sign in first".into(),
            ));
        }
        let scan_home = home.clone();
        let scan =
            tokio::task::spawn_blocking(move || logs::scan_home(&scan_home, chrono::Local::now()))
                .await
                .map_err(|_| ProviderError::Other("The DSH usage scan did not finish".into()))?;
        let wallets = account::fetch_wallets(&home).await;
        let headline = wallets
            .as_ref()
            .ok()
            .and_then(|w| w.normal.first())
            .map(money)
            .or_else(|| {
                scan.as_ref()
                    .ok()
                    .map(|s| format!("{} tok", count(s.today.total)))
            })
            .unwrap_or_else(|| "Unavailable".into());
        let mut usage =
            UsageSnapshot::new(RateWindow::with_details(0.0, None, None, Some(headline)))
                .with_login_method("Local DSH · DeepSeek account")
                .with_unavailable_rate_window(
                    "dsh-usage",
                    "DSH",
                    "The account reports no plan limit or reset time; these are recorded totals",
                );
        match wallets {
            Ok(wallets) => {
                for w in wallets.normal {
                    usage = row(
                        usage,
                        format!("dsh-balance-{}", w.currency),
                        format!("Topped-up balance ({})", w.currency),
                        money(&w),
                    );
                }
                for w in wallets.bonus {
                    usage = row(
                        usage,
                        format!("dsh-bonus-{}", w.currency),
                        format!("Bonus balance ({})", w.currency),
                        money(&w),
                    );
                }
            }
            Err(error) => usage = row(usage, "dsh-balance-status", "Account balance", error),
        }
        match scan {
            Ok(s) => {
                let prefix = if s.warnings.is_empty() {
                    ""
                } else {
                    "At least "
                };
                usage = row(
                    usage,
                    "dsh-today",
                    "Tokens today",
                    format!("{prefix}{}", count(s.today.total)),
                );
                usage = row(
                    usage,
                    "dsh-month",
                    "Tokens this month",
                    format!("{prefix}{}", count(s.month.total)),
                );
                usage = row(
                    usage,
                    "dsh-input",
                    "Input this month (uncached)",
                    count(s.month.input),
                );
                usage = row(
                    usage,
                    "dsh-output",
                    "Output this month",
                    count(s.month.output),
                );
                usage = row(
                    usage,
                    "dsh-cache",
                    "Cache reads this month",
                    count(s.month.cache_read),
                );
                if s.month.cache_write > 0 {
                    usage = row(
                        usage,
                        "dsh-cache-write",
                        "Cache writes this month",
                        count(s.month.cache_write),
                    );
                }
                usage = row(
                    usage,
                    "dsh-calls",
                    "Calls recorded this month",
                    count(s.month.calls),
                );
                let mut models = s.models.into_iter().collect::<Vec<_>>();
                models.sort_by_key(|(_, tokens)| std::cmp::Reverse(tokens.total));
                for (index, (model, tokens)) in models.into_iter().enumerate() {
                    usage = row(
                        usage,
                        format!("dsh-model-{index}"),
                        model,
                        count(tokens.total),
                    );
                }
                usage = row(
                    usage,
                    "dsh-sessions",
                    "Local sessions read",
                    count(s.sessions),
                );
                if !s.warnings.is_empty() {
                    usage = row(
                        usage,
                        "dsh-log-status",
                        "Some records not counted",
                        s.warnings.join("; "),
                    );
                }
            }
            Err(error) => usage = row(usage, "dsh-log-status", "Local usage unavailable", error),
        }
        Ok(ProviderFetchResult::new(
            usage,
            "DSH local records + DeepSeek account",
        ))
    }
}
#[cfg(test)]
mod tests;
