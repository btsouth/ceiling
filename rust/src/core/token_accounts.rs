//! Token Account Multi-Support
//!
//! Store and manage multiple accounts/tokens per provider.
//! Supports parallel fetching and account switching.

use crate::core::ProviderId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use uuid::Uuid;

/// How to inject a token into a fetch request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TokenInjection {
    /// Inject as Cookie header value
    CookieHeader,
    /// Inject as environment variable
    Environment { key: String },
}

/// Support definition for a provider's token accounts
#[derive(Debug, Clone)]
pub struct TokenAccountSupport {
    /// Display title for the UI
    pub title: &'static str,
    /// Subtitle/description for the UI
    pub subtitle: &'static str,
    /// Placeholder text for input field
    pub placeholder: &'static str,
    /// How tokens are injected
    pub injection: TokenInjection,
    /// Whether manual cookie source is required
    pub requires_manual_cookie_source: bool,
    /// Cookie name to use when normalizing (e.g., "sessionKey")
    pub cookie_name: Option<&'static str>,
}

impl TokenAccountSupport {
    /// Get token account support for a provider
    pub fn for_provider(provider: ProviderId) -> Option<Self> {
        match provider {
            ProviderId::Claude => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store Claude sessionKey cookies for settings-page usage. OAuth tokens are kept as a legacy fallback.",
                placeholder: "Paste sessionKey value or Cookie: sessionKey=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("sessionKey"),
            }),
            ProviderId::Zai => Some(TokenAccountSupport {
                title: "API tokens",
                subtitle: "Stored locally in token-accounts.json. Team usage can use workspace_id as organization|project.",
                placeholder: "Paste token...",
                injection: TokenInjection::Environment {
                    key: "Z_AI_API_KEY".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            ProviderId::Cursor => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Paste WorkosCursorSessionToken from cursor.com, or a bare session value / JWT. Automatic can also read the signed-in Cursor IDE session on disk.",
                placeholder: "WorkosCursorSessionToken=… or bare user_…%3A%3A… / JWT",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("WorkosCursorSessionToken"),
            }),
            ProviderId::OpenCode => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple OpenCode Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Factory => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Factory Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Alibaba => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Alibaba Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::AlibabaTokenPlan => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Alibaba Token Plan Cookie headers.",
                placeholder: "Cookie: cna=...; login_aliyunid_csrf=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::MiniMax => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple MiniMax Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Augment => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Augment Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Amp => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Amp Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Ollama => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Ollama Cookie headers or __Secure-session values.",
                placeholder: "__Secure-session value or Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("__Secure-session"),
            }),
            ProviderId::T3Chat => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple T3 Chat Cookie headers or full browser cURL captures.",
                placeholder: "Cookie: ... or curl ... -H 'Cookie: ...'",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Mistral => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Mistral Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Manus => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Manus session_id values.",
                placeholder: "session_id value or Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("session_id"),
            }),
            ProviderId::MiMo => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Xiaomi MiMo Cookie headers.",
                placeholder: "Cookie: api-platform_serviceToken=...; userId=...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::CommandCode => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Command Code Cookie headers or Better Auth values.",
                placeholder: "Cookie: __Secure-commandcode_prod_.session_token=... or better-auth value",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: Some("__Secure-better-auth.session_token"),
            }),
            ProviderId::Qoder => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Qoder Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Sakana => Some(TokenAccountSupport {
                title: "Session tokens",
                subtitle: "Store multiple Sakana Console Cookie headers.",
                placeholder: "Cookie: ...",
                injection: TokenInjection::CookieHeader,
                requires_manual_cookie_source: true,
                cookie_name: None,
            }),
            ProviderId::Copilot => Some(TokenAccountSupport {
                title: "GitHub accounts",
                subtitle: "Store GitHub OAuth tokens for Copilot plan usage.",
                placeholder: "Sign in with GitHub or paste a GitHub OAuth token...",
                injection: TokenInjection::Environment {
                    key: "GITHUB_TOKEN".to_string(),
                },
                requires_manual_cookie_source: false,
                cookie_name: None,
            }),
            // These providers don't support token accounts
            ProviderId::Codex
            | ProviderId::Gemini
            | ProviderId::Antigravity
            | ProviderId::Kiro
            | ProviderId::VertexAI
            | ProviderId::Kimi
            | ProviderId::KimiK2
            | ProviderId::JetBrains
            | ProviderId::Warp
            | ProviderId::AzureOpenAI
            | ProviderId::OpenRouter
            | ProviderId::NanoGPT
            | ProviderId::Infini
            | ProviderId::Perplexity
            | ProviderId::Abacus
            | ProviderId::OpenCodeGo
            | ProviderId::Kilo
            | ProviderId::Bedrock
            | ProviderId::Codebuff
            | ProviderId::DeepSeek
            | ProviderId::Windsurf
            | ProviderId::Doubao
            | ProviderId::Crof
            | ProviderId::StepFun
            | ProviderId::Venice
            | ProviderId::OpenAIApi
            | ProviderId::Grok
            | ProviderId::ElevenLabs
            | ProviderId::Deepgram
            | ProviderId::Groq
            | ProviderId::LLMProxy
            | ProviderId::Chutes
            | ProviderId::LiteLLM
            | ProviderId::Poe
            | ProviderId::Devin
            | ProviderId::Zed
            | ProviderId::CrossModel
            | ProviderId::Wayfinder
            | ProviderId::DeepSeekHarness => None,
        }
    }

    /// Check if a provider supports token accounts
    pub fn is_supported(provider: ProviderId) -> bool {
        Self::for_provider(provider).is_some()
    }

    /// Get environment override for a token
    pub fn env_override(provider: ProviderId, token: &str) -> Option<HashMap<String, String>> {
        let support = Self::for_provider(provider)?;
        match &support.injection {
            TokenInjection::Environment { key } => {
                let mut map = HashMap::new();
                map.insert(key.clone(), token.to_string());
                Some(map)
            }
            TokenInjection::CookieHeader => {
                // Check for Claude OAuth token
                if provider == ProviderId::Claude
                    && let Some(normalized) = Self::normalized_claude_oauth_token(token)
                    && Self::is_claude_oauth_token(&normalized)
                {
                    let mut map = HashMap::new();
                    map.insert("CODEXBAR_CLAUDE_OAUTH_TOKEN".to_string(), normalized);
                    return Some(map);
                }
                None
            }
        }
    }

    /// Normalize a cookie header for a provider
    pub fn normalized_cookie_header(provider: ProviderId, token: &str) -> String {
        if provider == ProviderId::Cursor
            && let Some(header) = crate::providers::cursor::normalize_cookie_header(token)
        {
            return header;
        }

        let trimmed = token.trim();
        let Some(support) = Self::for_provider(provider) else {
            return trimmed.to_string();
        };

        let Some(cookie_name) = support.cookie_name else {
            return trimmed.to_string();
        };

        let mut header = trimmed;
        let lower = header.to_lowercase();
        if lower.starts_with("cookie:") {
            header = header["cookie:".len()..].trim();
        }

        if header.contains('=') {
            return header.to_string();
        }

        format!("{}={}", cookie_name, header)
    }

    /// Check if a token is a Claude OAuth token
    pub fn is_claude_oauth_token(token: &str) -> bool {
        let Some(trimmed) = Self::normalized_claude_oauth_token(token) else {
            return false;
        };
        let lower = trimmed.to_lowercase();
        if lower.contains("cookie:") || trimmed.contains('=') {
            return false;
        }
        lower.starts_with("sk-ant-oat")
    }

    /// Normalize a Claude OAuth token
    fn normalized_claude_oauth_token(token: &str) -> Option<String> {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("bearer ") {
            Some(trimmed[7..].trim().to_string())
        } else {
            Some(trimmed.to_string())
        }
    }
}

/// A single token account for a provider
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenAccount {
    /// Unique identifier
    pub id: Uuid,
    /// User-provided label
    pub label: String,
    /// The token/cookie value
    pub token: String,
    /// When this account was added (Unix timestamp in seconds)
    pub added_at: i64,
    /// When this account was last used (Unix timestamp in seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
}

impl TokenAccount {
    /// Create a new token account
    pub fn new(label: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
            token: token.into(),
            added_at: Utc::now().timestamp(),
            last_used: None,
        }
    }

    /// Mark this account as used
    pub fn mark_used(&mut self) {
        self.last_used = Some(Utc::now().timestamp());
    }

    /// Get display name
    pub fn display_name(&self) -> &str {
        &self.label
    }

    /// Get added_at as DateTime
    pub fn added_at_datetime(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.added_at, 0).unwrap_or_else(Utc::now)
    }

    /// Get last_used as DateTime
    pub fn last_used_datetime(&self) -> Option<DateTime<Utc>> {
        self.last_used
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
    }
}

/// Account data for a provider
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderAccountData {
    /// File format version
    #[serde(default = "default_version")]
    pub version: u32,
    /// List of accounts
    pub accounts: Vec<TokenAccount>,
    /// Index of the active account
    #[serde(default)]
    pub active_index: usize,
}

fn default_version() -> u32 {
    1
}

impl ProviderAccountData {
    /// Create new empty account data
    pub fn new() -> Self {
        Self {
            version: 1,
            accounts: Vec::new(),
            active_index: 0,
        }
    }

    /// Get the clamped active index
    pub fn clamped_active_index(&self) -> usize {
        if self.accounts.is_empty() {
            return 0;
        }
        self.active_index.min(self.accounts.len() - 1)
    }

    /// Get the active account
    pub fn active_account(&self) -> Option<&TokenAccount> {
        self.accounts.get(self.clamped_active_index())
    }

    /// Get the active account mutably
    pub fn active_account_mut(&mut self) -> Option<&mut TokenAccount> {
        let idx = self.clamped_active_index();
        self.accounts.get_mut(idx)
    }

    /// Add a new account
    pub fn add_account(&mut self, account: TokenAccount) {
        self.accounts.push(account);
    }

    /// Remove an account by ID
    pub fn remove_account(&mut self, id: Uuid) -> Option<TokenAccount> {
        let index = self.accounts.iter().position(|a| a.id == id)?;
        // Track the seat by identity. `active_index` is positional, so only
        // clamping it when it runs past the end silently promotes a different
        // account whenever something before it is removed. Mirrors
        // `DirectoryAccountData::remove_account`.
        let active_id = self.active_account().map(|account| account.id);
        let removed = self.accounts.remove(index);

        self.active_index = match active_id {
            Some(active_id) if active_id != removed.id => self
                .accounts
                .iter()
                .position(|account| account.id == active_id)
                .unwrap_or(0),
            // The active account itself went away: fall back to its neighbor.
            _ => index.min(self.accounts.len().saturating_sub(1)),
        };

        Some(removed)
    }

    /// Set the active account by index
    pub fn set_active(&mut self, index: usize) {
        self.active_index = index.min(self.accounts.len().saturating_sub(1));
    }

    /// Set the active account by ID
    pub fn set_active_by_id(&mut self, id: Uuid) -> bool {
        if let Some(pos) = self.accounts.iter().position(|a| a.id == id) {
            self.active_index = pos;
            true
        } else {
            false
        }
    }

    /// Check if this provider has multiple accounts
    pub fn has_multiple(&self) -> bool {
        self.accounts.len() > 1
    }

    /// Get account count
    pub fn count(&self) -> usize {
        self.accounts.len()
    }
}

/// File format for storing all provider accounts
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenAccountsFile {
    version: u32,
    providers: HashMap<String, ProviderAccountData>,
}

/// Token account store for persisting accounts to disk
pub struct TokenAccountStore {
    file_path: PathBuf,
}

/// Errors that can occur with token account storage
#[derive(Debug, thiserror::Error)]
pub enum TokenAccountError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl TokenAccountStore {
    /// Create a new store with the default path
    pub fn new() -> Self {
        Self {
            file_path: Self::default_path(),
        }
    }

    /// Create a store with a custom path
    pub fn with_path(path: PathBuf) -> Self {
        Self { file_path: path }
    }

    /// Get the default storage path
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|dir| dir.join("CodexBar"))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".codexbar")
            })
            .join("token-accounts.json")
    }

    /// Read the file exactly as stored, including provider ids this build does
    /// not resolve.
    fn load_raw(&self) -> Result<HashMap<String, ProviderAccountData>, TokenAccountError> {
        if !self.file_path.exists() {
            return Ok(HashMap::new());
        }

        let data = crate::secure_file::read_string(&self.file_path)?;
        let file: TokenAccountsFile = serde_json::from_str(&data)?;
        Ok(file.providers)
    }

    /// Load all accounts from disk, keyed by the providers this build knows.
    ///
    /// Read-only. Mutations must go through [`Self::try_update_provider`];
    /// `load` then [`Self::save`] can overwrite a concurrent edit.
    ///
    /// Ids that do not resolve are omitted here but are **not** forgotten —
    /// [`save`](Self::save) reads them back off disk and preserves them.
    pub fn load(&self) -> Result<HashMap<ProviderId, ProviderAccountData>, TokenAccountError> {
        let mut result = HashMap::new();
        for (key, value) in self.load_raw()? {
            if let Some(provider) = ProviderId::from_cli_name(&key) {
                result.insert(provider, value);
            }
        }
        Ok(result)
    }

    /// Replace every recognized provider's on-disk snapshot.
    ///
    /// For a read-modify-write, use [`Self::try_update_provider`] so the load
    /// stays under the same lock as the save.
    ///
    /// `accounts` is authoritative for every provider this build resolves, so
    /// omitting one deletes it. Credentials stored under an id this build does
    /// **not** resolve are carried over from the existing file rather than
    /// dropped: `save` replaces the whole document, so a downgrade (or a
    /// renamed `cli_name`) would otherwise destroy another build's accounts on
    /// the first unrelated add/remove/activate (SBS-628).
    pub fn save(
        &self,
        accounts: &HashMap<ProviderId, ProviderAccountData>,
    ) -> Result<(), TokenAccountError> {
        with_store_lock(|| self.save_unlocked(accounts))
    }

    pub(crate) fn save_unlocked(
        &self,
        accounts: &HashMap<ProviderId, ProviderAccountData>,
    ) -> Result<(), TokenAccountError> {
        // Read before create_dir_all/write so an undecodable existing file
        // fails closed instead of being replaced by a partial one.
        let mut providers: HashMap<String, ProviderAccountData> = self
            .load_raw()?
            .into_iter()
            .filter(|(key, _)| ProviderId::from_cli_name(key).is_none())
            .collect();

        // Ensure directory exists
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        providers.extend(
            accounts
                .iter()
                .map(|(k, v)| (k.cli_name().to_string(), v.clone())),
        );

        let file = TokenAccountsFile {
            version: 1,
            providers,
        };

        let json = serde_json::to_string_pretty(&file)?;
        crate::secure_file::write_string(&self.file_path, &json)?;
        Ok(())
    }

    /// Ensure the accounts file exists
    pub fn ensure_exists(&self) -> Result<PathBuf, TokenAccountError> {
        if self.file_path.exists() {
            return Ok(self.file_path.clone());
        }
        self.save(&HashMap::new())?;
        Ok(self.file_path.clone())
    }

    /// Load accounts for a specific provider.
    ///
    /// Read-only. Mutations must go through [`Self::try_update_provider`].
    pub fn load_provider(
        &self,
        provider: ProviderId,
    ) -> Result<ProviderAccountData, TokenAccountError> {
        let all = self.load()?;
        Ok(all.get(&provider).cloned().unwrap_or_default())
    }

    /// Replace one provider's accounts under the state lock.
    ///
    /// Safe for a freshly built snapshot. Do not `load_provider`, mutate, then
    /// call this — that snapshot can be stale. Use [`Self::try_update_provider`].
    pub fn save_provider(
        &self,
        provider: ProviderId,
        data: &ProviderAccountData,
    ) -> Result<(), TokenAccountError> {
        with_store_lock(|| {
            let mut all = self.load()?;
            all.insert(provider, data.clone());
            self.save_unlocked(&all)
        })
    }

    /// Mutate one provider's latest account snapshot as a single locked
    /// transaction, preserving other providers and unrecognized provider ids.
    pub fn try_update_provider<T>(
        &self,
        provider: ProviderId,
        operation: impl FnOnce(&mut ProviderAccountData) -> Result<T, String>,
    ) -> anyhow::Result<(ProviderAccountData, T)> {
        with_store_lock(|| {
            let mut all = self.load()?;
            let mut data = all.get(&provider).cloned().unwrap_or_default();
            let original = data.clone();
            let result = operation(&mut data).map_err(io::Error::other)?;
            // `or_default` would insert an empty provider and write a missing
            // file even when the mutation added nothing.
            if data != original {
                all.insert(provider, data.clone());
                self.save_unlocked(&all)?;
            }
            Ok((data, result))
        })
        .map_err(Into::into)
    }
}

fn with_store_lock<T>(
    operation: impl FnOnce() -> Result<T, TokenAccountError>,
) -> Result<T, TokenAccountError> {
    let mut op_err = None;
    crate::secure_file::with_state_write_lock(|| match operation() {
        Ok(value) => Ok(value),
        Err(err) => {
            op_err = Some(err);
            Err(io::Error::other("token account store operation failed"))
        }
    })
    .map_err(|lock_err| op_err.unwrap_or(TokenAccountError::Io(lock_err)))
}

impl Default for TokenAccountStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Override for temporarily using a different token during fetch
#[derive(Debug, Clone)]
pub struct TokenAccountOverride {
    /// The provider being overridden
    pub provider: ProviderId,
    /// The account being used
    pub account: TokenAccount,
    /// Environment variables to set
    pub env_override: Option<HashMap<String, String>>,
    /// Cookie header to use
    pub cookie_header: Option<String>,
}

impl TokenAccountOverride {
    /// Create an override from an account
    pub fn from_account(provider: ProviderId, account: TokenAccount) -> Self {
        let env_override = TokenAccountSupport::env_override(provider, &account.token);
        let cookie_header = if env_override.is_none() {
            Some(TokenAccountSupport::normalized_cookie_header(
                provider,
                &account.token,
            ))
        } else {
            None
        };

        Self {
            provider,
            account,
            env_override,
            cookie_header,
        }
    }
}

/// Maximum number of accounts to fetch per provider
pub const MAX_ACCOUNTS_PER_FETCH: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;

    /// SBS-618/639: `active_index` is positional, so removing an account that
    /// sits *before* the active one used to leave the index pointing at a
    /// different account. Nothing errored — the user simply started fetching
    /// with the wrong seat.
    #[test]
    fn removing_an_earlier_account_keeps_the_same_account_active() {
        let mut data = ProviderAccountData::default();
        data.add_account(TokenAccount::new("personal", "sessionKey=personal"));
        data.add_account(TokenAccount::new("work", "sessionKey=work"));
        data.add_account(TokenAccount::new("school", "sessionKey=school"));

        let work = data.accounts[1].id;
        data.set_active_by_id(work);

        let personal = data.accounts[0].id;
        data.remove_account(personal).expect("remove personal");

        assert_eq!(
            data.active_account().map(|account| account.id),
            Some(work),
            "the selected seat must survive removal of an earlier account"
        );
        assert_eq!(
            data.active_account().map(|a| a.label.as_str()),
            Some("work")
        );
    }

    #[test]
    fn removing_the_active_account_falls_back_to_its_neighbor() {
        let mut data = ProviderAccountData::default();
        data.add_account(TokenAccount::new("personal", "sessionKey=personal"));
        data.add_account(TokenAccount::new("work", "sessionKey=work"));
        data.add_account(TokenAccount::new("school", "sessionKey=school"));

        let work = data.accounts[1].id;
        data.set_active_by_id(work);
        data.remove_account(work).expect("remove work");

        // The account that shifted into the freed slot takes over.
        assert_eq!(
            data.active_account().map(|a| a.label.as_str()),
            Some("school")
        );

        // Removing the last remaining active account clamps rather than
        // pointing past the end.
        let school = data.accounts[1].id;
        data.set_active_by_id(school);
        data.remove_account(school).expect("remove school");
        assert_eq!(
            data.active_account().map(|a| a.label.as_str()),
            Some("personal")
        );

        let personal = data.accounts[0].id;
        data.remove_account(personal).expect("remove personal");
        assert!(data.accounts.is_empty());
        assert!(data.active_account().is_none());
    }

    /// SBS-628: `load` filters the on-disk map through `from_cli_name`, and
    /// `save`/`save_provider` rewrite the whole document from that filtered
    /// map. Without carrying unresolvable ids across, any add/remove/activate
    /// permanently erases credentials stored by a build that knows a provider
    /// this one does not (downgrade, renamed `cli_name`, fork).
    #[test]
    fn save_provider_preserves_credentials_for_unrecognized_providers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token-accounts.json");
        let store = TokenAccountStore::with_path(path.clone());

        // A file written by a build that knows `brand_new_provider`, alongside
        // one this build does resolve.
        let mut claude = ProviderAccountData::default();
        claude.add_account(TokenAccount::new("personal", "sessionKey=known"));
        store
            .save_provider(ProviderId::Claude, &claude)
            .expect("seed known provider");

        let raw = crate::secure_file::read_string(&path).expect("read seeded file");
        let mut file: serde_json::Value = serde_json::from_str(&raw).expect("parse seeded file");
        let unknown = serde_json::json!({
            "version": 1,
            "accounts": [{
                "id": Uuid::new_v4(),
                "label": "future",
                "token": "sessionKey=future-secret",
                "added_at": 1_754_700_000_i64,
            }],
            "active_index": 0,
        });
        file["providers"]["brand_new_provider"] = unknown;
        crate::secure_file::write_string(&path, &file.to_string()).expect("write mixed file");

        // Ordinary user action on an unrelated, recognized provider.
        let mut cursor = ProviderAccountData::default();
        cursor.add_account(TokenAccount::new("work", "WorkosCursorSessionToken=abc"));
        store
            .save_provider(ProviderId::Cursor, &cursor)
            .expect("save unrelated provider");

        let after = crate::secure_file::read_string(&path).expect("read after save");
        let after: serde_json::Value = serde_json::from_str(&after).expect("parse after save");
        assert!(
            after["providers"]["brand_new_provider"]["accounts"][0]["token"]
                .as_str()
                .is_some_and(|token| token.contains("future-secret")),
            "credentials under an unrecognized provider id must survive an \
             unrelated save; file was {after}"
        );
        // And the recognized providers are both intact.
        let loaded = store.load().expect("load after save");
        assert_eq!(loaded[&ProviderId::Claude].accounts.len(), 1);
        assert_eq!(loaded[&ProviderId::Cursor].accounts.len(), 1);
    }

    #[test]
    fn concurrent_provider_saves_both_survive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token-accounts.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        let writers: Vec<_> = [ProviderId::Claude, ProviderId::Cursor]
            .into_iter()
            .map(|provider| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut data = ProviderAccountData::default();
                    data.add_account(TokenAccount::new(
                        format!("{provider:?}"),
                        format!("secret-{provider:?}"),
                    ));
                    barrier.wait();
                    TokenAccountStore::with_path(path)
                        .save_provider(provider, &data)
                        .expect("concurrent provider save");
                })
            })
            .collect();
        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let stored = TokenAccountStore::with_path(path).load().expect("reload");
        assert!(stored.contains_key(&ProviderId::Claude));
        assert!(stored.contains_key(&ProviderId::Cursor));
    }

    #[test]
    fn transactional_updates_preserve_two_same_provider_adds() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token-accounts.json");
        let barrier = Arc::new(Barrier::new(3));
        let writers: Vec<_> = ["personal", "work"]
            .into_iter()
            .map(|label| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    TokenAccountStore::with_path(path)
                        .try_update_provider(ProviderId::Claude, |data| {
                            data.add_account(TokenAccount::new(
                                label,
                                format!("sessionKey={label}"),
                            ));
                            Ok(())
                        })
                        .expect("transactional token account update");
                })
            })
            .collect();

        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let stored = TokenAccountStore::with_path(path)
            .load_provider(ProviderId::Claude)
            .expect("reload");
        assert_eq!(stored.count(), 2);
        assert!(
            stored
                .accounts
                .iter()
                .any(|entry| entry.label == "personal")
        );
        assert!(stored.accounts.iter().any(|entry| entry.label == "work"));
    }

    #[test]
    fn no_op_try_update_provider_leaves_a_missing_store_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token-accounts.json");
        let store = TokenAccountStore::with_path(path.clone());

        store
            .try_update_provider(ProviderId::Claude, |_| Ok(()))
            .expect("no-op token account update");

        assert!(
            !path.exists(),
            "a no-op must not create an empty token-accounts file"
        );
    }

    /// Removing a known provider must still remove it — preservation applies
    /// only to ids this build cannot resolve.
    #[test]
    fn save_still_deletes_recognized_providers_left_out_of_the_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TokenAccountStore::with_path(dir.path().join("token-accounts.json"));

        let mut claude = ProviderAccountData::default();
        claude.add_account(TokenAccount::new("personal", "sessionKey=known"));
        store
            .save_provider(ProviderId::Claude, &claude)
            .expect("seed");

        store.save(&HashMap::new()).expect("save empty");
        assert!(store.load().expect("load").is_empty());
    }

    #[test]
    fn test_token_account_support() {
        assert!(TokenAccountSupport::is_supported(ProviderId::Claude));
        assert!(TokenAccountSupport::is_supported(ProviderId::Cursor));
        assert!(TokenAccountSupport::is_supported(ProviderId::Copilot));
        assert!(!TokenAccountSupport::is_supported(ProviderId::Codex));
        assert!(!TokenAccountSupport::is_supported(ProviderId::Gemini));
    }

    #[test]
    fn test_claude_oauth_detection() {
        assert!(TokenAccountSupport::is_claude_oauth_token(
            "sk-ant-oat01-abc123"
        ));
        assert!(TokenAccountSupport::is_claude_oauth_token(
            "Bearer sk-ant-oat01-abc123"
        ));
        assert!(!TokenAccountSupport::is_claude_oauth_token(
            "sessionKey=abc123"
        ));
        assert!(!TokenAccountSupport::is_claude_oauth_token(
            "Cookie: foo=bar"
        ));
    }

    #[test]
    fn test_normalize_cookie_header() {
        let header =
            TokenAccountSupport::normalized_cookie_header(ProviderId::Claude, "abc123token");
        assert_eq!(header, "sessionKey=abc123token");

        let header = TokenAccountSupport::normalized_cookie_header(
            ProviderId::Claude,
            "sessionKey=already_formatted",
        );
        assert_eq!(header, "sessionKey=already_formatted");

        let header = TokenAccountSupport::normalized_cookie_header(ProviderId::Ollama, "abc123");
        assert_eq!(header, "__Secure-session=abc123");
    }

    #[test]
    fn test_provider_account_data() {
        let mut data = ProviderAccountData::new();
        assert_eq!(data.clamped_active_index(), 0);
        assert!(data.active_account().is_none());

        let account = TokenAccount::new("Test", "token123");
        let id = account.id;
        data.add_account(account);

        assert_eq!(data.count(), 1);
        assert!(data.active_account().is_some());
        assert_eq!(data.active_account().unwrap().label, "Test");

        data.remove_account(id);
        assert_eq!(data.count(), 0);
    }

    #[test]
    fn test_multiple_accounts() {
        let mut data = ProviderAccountData::new();
        data.add_account(TokenAccount::new("Account 1", "token1"));
        data.add_account(TokenAccount::new("Account 2", "token2"));

        assert!(data.has_multiple());
        assert_eq!(data.active_account().unwrap().label, "Account 1");

        data.set_active(1);
        assert_eq!(data.active_account().unwrap().label, "Account 2");
    }
}
