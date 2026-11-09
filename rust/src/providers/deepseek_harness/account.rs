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
    let doc: CredentialDocument = serde_yaml_ng::from_str(text)
        .map_err(|_| "The DSH credentials file could not be parsed".to_string())?;
    if doc.version != 1 {
        return Err("This DSH credentials file version is not supported yet".into());
    }
    let Some(record) = doc.records.get("deepseek-account-platform/default") else {
        return Ok(None);
    };
    let payload = &record["payload"];
    if record["kind"] != "grant" || payload["version"] != 1 || payload["issuer"] != ORIGIN {
        return Err(
            "The DSH sign-in is not from platform.deepseek.com, so the balance is unavailable"
                .into(),
        );
    }
    let token = payload["token"]
        .as_str()
        .filter(|v| {
            !v.is_empty() && v.len() < 65536 && v.bytes().all(|b| (0x21..=0x7e).contains(&b))
        })
        .ok_or("The DSH sign-in token is invalid")?;
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
        return Err(
            "The balance request failed. Check that you are signed in to DeepSeek Harness".into(),
        );
    }
    let data = value
        .pointer("/data/biz_data")
        .ok_or("The balance response has no data")?;
    let parse = |key: &str| -> Result<Vec<Wallet>, String> {
        data[key]
            .as_array()
            .ok_or("The balance response is not in the expected format")?
            .iter()
            .map(|row| {
                let currency = row["currency"]
                    .as_str()
                    .filter(|v| matches!(*v, "CNY" | "USD"))
                    .ok_or("The balance response has an unknown currency")?;
                let balance = row["balance"]
                    .as_str()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .ok_or("The balance response has an invalid amount")?;
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
    let file = std::fs::File::open(&path).map_err(|_| "No DSH sign-in found".to_string())?;
    if file
        .metadata()
        .map_err(|_| "Could not read the DSH sign-in")?
        .len()
        > MAX_CREDENTIAL_BYTES
    {
        return Err("The DSH credentials file is over the size limit".into());
    }
    use std::io::Read;
    let mut text = String::new();
    file.take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Could not read the DSH sign-in")?;
    if text.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err("The DSH credentials file is over the size limit".into());
    }
    let grant = parse_grant(&text)?.ok_or("Sign in to DeepSeek Harness to see your balance")?;
    let client = crate::core::credentialed_http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|_| "Could not set up the balance request")?;
    let mut auth_header = reqwest::header::HeaderValue::from_str(&grant.token)
        .map_err(|_| "The DSH sign-in token is invalid")?;
    auth_header.set_sensitive(true);
    let mut response = client
        .get(format!("{ORIGIN}/api/v0/users/get_user_summary"))
        .header("x-dsh-auth-token", auth_header)
        // Same platform values DSH's own account client sends.
        .header(
            "x-client-platform",
            if cfg!(windows) {
                "desktop-win"
            } else if cfg!(target_os = "macos") {
                "desktop-mac"
            } else {
                "web"
            },
        )
        .header("x-client-version", "0.2.0")
        .header("x-client-locale", "zh_CN")
        .header("x-client-bundle-id", "")
        .header(
            "x-client-timezone-offset",
            chrono::Local::now().offset().local_minus_utc().to_string(),
        )
        .send()
        .await
        .map_err(|_| "Could not reach DeepSeek for the balance. Token counts are still shown")?;
    if !response.status().is_success() {
        return Err(format!(
            "Balance unavailable (HTTP {}). Check that you are signed in to DeepSeek Harness",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The balance response could not be read")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("The balance response is over the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let data: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "The balance response is not in the expected format")?;
    let wallets = parse_wallets(&data)?;
    // A sign-out/account switch during the request must not publish the old wallet.
    let mut current = String::new();
    std::fs::File::open(&path)
        .map_err(|_| "The DSH sign-in changed. Refresh to update")?
        .take(MAX_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut current)
        .map_err(|_| "The DSH sign-in changed. Refresh to update")?;
    if current.len() as u64 > MAX_CREDENTIAL_BYTES
        || parse_grant(&current)?.is_none_or(|g| g.token != grant.token)
    {
        return Err("The DSH account changed. Refresh to update the balance".into());
    }
    Ok(wallets)
}
