//! Read-only use of DSH's existing account grant. Never expose parser excerpts.
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

const ORIGIN: &str = "https://platform.deepseek.com";
const MAX_CREDENTIAL_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct CredentialDocument {
    version: u32,
    #[serde(default)]
    records: BTreeMap<String, Value>,
}
// Deliberately no Debug/Serialize implementation for account credentials.
pub(super) struct Grant {
    token: String,
}

pub(super) fn parse_grant(text: &str) -> Result<Option<Grant>, String> {
    let doc: CredentialDocument =
        serde_yaml_ng::from_str(text).map_err(|_| "DSH 登录文件格式无法读取".to_string())?;
    if doc.version != 1 {
        return Err("暂不支持此 DSH 登录文件版本".into());
    }
    let Some(record) = doc.records.get("deepseek-account-platform/default") else {
        return Ok(None);
    };
    let payload = &record["payload"];
    if record["kind"] != "grant" || payload["version"] != 1 || payload["issuer"] != ORIGIN {
        return Err("DSH 登录来源与官方平台不匹配，余额暂不可用".into());
    }
    let token = payload["token"]
        .as_str()
        .filter(|v| {
            !v.is_empty() && v.len() < 65536 && v.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or("DSH 登录凭证无效")?;
    Ok(Some(Grant {
        token: token.to_string(),
    }))
}
#[derive(Debug)]
pub(super) struct Wallet {
    pub currency: String,
    pub balance: f64,
}
pub(super) struct Wallets {
    pub normal: Vec<Wallet>,
    pub bonus: Vec<Wallet>,
}
pub(super) fn parse_wallets(value: &Value) -> Result<Wallets, String> {
    if value["code"] != 0 || value.pointer("/data/biz_code") != Some(&Value::from(0)) {
        return Err("DSH 账号余额请求未成功，请在 Harness 中确认登录".into());
    }
    let data = value.pointer("/data/biz_data").ok_or("余额响应缺少数据")?;
    let parse = |key: &str| -> Result<Vec<Wallet>, String> {
        data[key]
            .as_array()
            .ok_or("余额响应格式无效")?
            .iter()
            .map(|row| {
                let currency = row["currency"]
                    .as_str()
                    .filter(|v| matches!(*v, "CNY" | "USD"))
                    .ok_or("余额币种无效")?;
                let balance = row["balance"]
                    .as_str()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .ok_or("余额数字无效")?;
                Ok(Wallet {
                    currency: currency.into(),
                    balance,
                })
            })
            .collect()
    };
    Ok(Wallets {
        normal: parse("normal_wallets")?,
        bonus: parse("bonus_wallets")?,
    })
}

pub(super) async fn fetch_wallets(home: &Path) -> Result<Wallets, String> {
    let path = home.join(".credentials.yaml");
    let file = std::fs::File::open(&path).map_err(|_| "未找到 DSH 账号登录状态".to_string())?;
    if file.metadata().map_err(|_| "无法读取 DSH 登录状态")?.len() > MAX_CREDENTIAL_BYTES {
        return Err("DSH 登录文件异常，余额暂不可用".into());
    }
    use std::io::Read;
    let mut text = String::new();
    file.take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "无法读取 DSH 登录状态")?;
    if text.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err("DSH 登录文件超过读取上限".into());
    }
    let grant = parse_grant(&text)?.ok_or("请在 DeepSeek Harness 中登录账号")?;
    let client = crate::core::credentialed_http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|_| "无法建立 DSH 余额连接")?;
    let mut auth_header =
        reqwest::header::HeaderValue::from_str(&grant.token).map_err(|_| "DSH 登录凭证无效")?;
    auth_header.set_sensitive(true);
    let mut response = client
        .get(format!("{ORIGIN}/api/v0/users/get_user_summary"))
        .header("x-dsh-auth-token", auth_header)
        .header("x-client-platform", "desktop-win")
        .header("x-client-version", "0.2.0")
        .header("x-client-locale", "zh_CN")
        .header("x-client-bundle-id", "")
        .header(
            "x-client-timezone-offset",
            chrono::Local::now().offset().local_minus_utc().to_string(),
        )
        .send()
        .await
        .map_err(|_| "余额暂时无法连接，Token 统计仍可查看")?;
    if !response.status().is_success() {
        return Err(format!(
            "账号余额暂不可用（HTTP {}），请在 DSH 中确认登录",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "余额响应读取失败")? {
        if bytes.len() + chunk.len() > 65536 {
            return Err("余额响应超过读取上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| "余额响应格式无效")?;
    let wallets = parse_wallets(&data)?;
    // A sign-out/account switch during the request must not publish the old wallet.
    let mut current = String::new();
    std::fs::File::open(&path)
        .map_err(|_| "DSH 登录状态已改变，请刷新")?
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut current)
        .map_err(|_| "DSH 登录状态已改变，请刷新")?;
    if current.len() as u64 > MAX_CREDENTIAL_BYTES
        || parse_grant(&current)?.is_none_or(|g| g.token != grant.token)
    {
        return Err("DSH 账号已切换，请刷新余额".into());
    }
    Ok(wallets)
}
