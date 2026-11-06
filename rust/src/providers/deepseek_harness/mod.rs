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
            .ok_or_else(|| ProviderError::NotInstalled("无法定位 DSH 用户目录".into()))?;
        if !home.exists() {
            return Err(ProviderError::NotInstalled(
                "未找到 DeepSeek Harness，请先运行并登录".into(),
            ));
        }
        let scan_home = home.clone();
        let scan =
            tokio::task::spawn_blocking(move || logs::scan_home(&scan_home, chrono::Local::now()))
                .await
                .map_err(|_| ProviderError::Other("DSH 用量读取任务未完成".into()))?;
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
            .unwrap_or_else(|| "暂不可用".into());
        let mut usage =
            UsageSnapshot::new(RateWindow::with_details(0.0, None, None, Some(headline)))
                .with_login_method("本机 DSH · 官方账号")
                .with_unavailable_rate_window(
                    "dsh-usage",
                    "DSH",
                    "账号没有提供套餐上限和重置时间；以下为实际记录的统计",
                );
        match wallets {
            Ok(wallets) => {
                for w in wallets.normal {
                    usage = row(
                        usage,
                        format!("dsh-balance-{}", w.currency),
                        format!("充值余额（{}）", w.currency),
                        money(&w),
                    );
                }
                for w in wallets.bonus {
                    usage = row(
                        usage,
                        format!("dsh-bonus-{}", w.currency),
                        format!("赠送余额（{}）", w.currency),
                        money(&w),
                    );
                }
            }
            Err(error) => usage = row(usage, "dsh-balance-status", "账户余额", error),
        }
        match scan {
            Ok(s) => {
                let prefix = if s.warnings.is_empty() { "" } else { "至少 " };
                usage = row(
                    usage,
                    "dsh-today",
                    "今日 Token",
                    format!("{prefix}{}", count(s.today.total)),
                );
                usage = row(
                    usage,
                    "dsh-month",
                    "本月 Token",
                    format!("{prefix}{}", count(s.month.total)),
                );
                usage = row(
                    usage,
                    "dsh-input",
                    "本月输入（未缓存）",
                    count(s.month.input),
                );
                usage = row(usage, "dsh-output", "本月输出", count(s.month.output));
                usage = row(
                    usage,
                    "dsh-cache",
                    "本月缓存读取",
                    count(s.month.cache_read),
                );
                if s.month.cache_write > 0 {
                    usage = row(
                        usage,
                        "dsh-cache-write",
                        "本月缓存写入",
                        count(s.month.cache_write),
                    );
                }
                usage = row(usage, "dsh-calls", "本月已记录调用", count(s.month.calls));
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
                usage = row(usage, "dsh-sessions", "已读取本机会话", count(s.sessions));
                if !s.warnings.is_empty() {
                    usage = row(
                        usage,
                        "dsh-log-status",
                        "部分记录暂未计入",
                        s.warnings.join("；"),
                    );
                }
            }
            Err(error) => usage = row(usage, "dsh-log-status", "本地用量暂不可用", error),
        }
        Ok(ProviderFetchResult::new(usage, "DSH 本地记录 + 官方账户"))
    }
}
#[cfg(test)]
mod tests;
