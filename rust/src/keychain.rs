//! OS keychain access gated by settings flags (SBS-1023).
//!
//! `disable_keychain_access` skips every keyring read and write.
//! Claude's `avoid_keychain_prompts` additionally skips Claude keyring
//! reads and token writes. Both prefer skip / fail-closed over prompting.

use crate::settings::Settings;
use thiserror::Error;

/// Which settings flags apply to a keychain operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Global reads/writes (`disable_keychain_access`).
    Any,
    /// Claude credential reads/writes (global flag or `avoid_keychain_prompts`).
    Claude,
}

/// Why a keychain operation did not return a secret.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum Error {
    #[error("keychain access is disabled (SBS-1023)")]
    Disabled,
    #[error("keychain entry not found")]
    NotFound,
    #[error("keychain storage error: {0}")]
    Storage(String),
}

/// Whether `settings` allows keychain access for `scope`.
pub(crate) fn allowed_for(settings: &Settings, scope: Scope) -> bool {
    match scope {
        Scope::Any => settings.keychain_access_allowed(),
        Scope::Claude => settings.claude_keychain_access_allowed(),
    }
}

/// Live settings snapshot used by UI and CLI credential paths.
pub(crate) fn allowed(scope: Scope) -> bool {
    #[cfg(test)]
    if let Some(override_allowed) = test_backend::allowed_override(scope) {
        return override_allowed;
    }
    allowed_for(&Settings::load(), scope)
}

/// Read a non-empty secret, or `None` when disabled, missing, or unreadable.
pub(crate) fn get_secret(scope: Scope, service: &str, user: &str) -> Option<String> {
    match get_password(scope, service, user) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

pub(crate) fn get_password(scope: Scope, service: &str, user: &str) -> Result<String, Error> {
    operate(scope, service, user, Operation::Get)
}

pub(crate) fn set_password(
    scope: Scope,
    service: &str,
    user: &str,
    value: &str,
) -> Result<(), Error> {
    operate(scope, service, user, Operation::Set(value))?;
    Ok(())
}

pub(crate) fn delete_credential(scope: Scope, service: &str, user: &str) -> Result<(), Error> {
    operate(scope, service, user, Operation::Delete)?;
    Ok(())
}

enum Operation<'a> {
    Get,
    Set(&'a str),
    Delete,
}

fn operate(
    scope: Scope,
    service: &str,
    user: &str,
    operation: Operation<'_>,
) -> Result<String, Error> {
    if !allowed(scope) {
        return Err(Error::Disabled);
    }
    #[cfg(test)]
    if let Some(result) = test_backend::operate(service, user, &operation) {
        return result;
    }
    os_operate(service, user, operation)
}

fn os_operate(service: &str, user: &str, operation: Operation<'_>) -> Result<String, Error> {
    let entry =
        keyring::Entry::new(service, user).map_err(|error| Error::Storage(error.to_string()))?;
    match operation {
        Operation::Get => entry.get_password().map_err(map_keyring_error),
        Operation::Set(value) => {
            entry
                .set_password(value)
                .map_err(|error| Error::Storage(error.to_string()))?;
            Ok(String::new())
        }
        Operation::Delete => match entry.delete_credential() {
            Ok(()) => Ok(String::new()),
            Err(keyring::Error::NoEntry) => Err(Error::NotFound),
            Err(error) => Err(Error::Storage(error.to_string())),
        },
    }
}

fn map_keyring_error(error: keyring::Error) -> Error {
    match error {
        keyring::Error::NoEntry => Error::NotFound,
        other => Error::Storage(other.to_string()),
    }
}

#[cfg(test)]
mod test_backend {
    use super::{Error, Operation, Scope};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    thread_local! {
        static ANY_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
        static CLAUDE_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
        static STORE: RefCell<Option<HashMap<(String, String), String>>> =
            const { RefCell::new(None) };
    }

    pub(super) fn allowed_override(scope: Scope) -> Option<bool> {
        match scope {
            Scope::Any => ANY_OVERRIDE.with(Cell::get),
            Scope::Claude => CLAUDE_OVERRIDE
                .with(Cell::get)
                .or_else(|| ANY_OVERRIDE.with(Cell::get)),
        }
    }

    pub(super) fn operate(
        service: &str,
        user: &str,
        operation: &Operation<'_>,
    ) -> Option<Result<String, Error>> {
        STORE.with(|store| {
            let mut store = store.borrow_mut();
            let map = store.as_mut()?;
            let key = (service.to_string(), user.to_string());
            Some(match operation {
                Operation::Get => map.get(&key).cloned().ok_or(Error::NotFound),
                Operation::Set(value) => {
                    map.insert(key, (*value).to_string());
                    Ok(String::new())
                }
                Operation::Delete => match map.remove(&key) {
                    Some(_) => Ok(String::new()),
                    None => Err(Error::NotFound),
                },
            })
        })
    }

    pub(crate) struct OverrideGuard {
        previous_any: Option<bool>,
        previous_claude: Option<bool>,
        previous_store: Option<HashMap<(String, String), String>>,
    }

    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            ANY_OVERRIDE.with(|cell| cell.set(self.previous_any));
            CLAUDE_OVERRIDE.with(|cell| cell.set(self.previous_claude));
            STORE.with(|store| {
                *store.borrow_mut() = self.previous_store.take();
            });
        }
    }

    pub(crate) fn with_state(
        any: Option<bool>,
        claude: Option<bool>,
        store: Option<HashMap<(String, String), String>>,
    ) -> OverrideGuard {
        let previous_any = ANY_OVERRIDE.with(|cell| cell.replace(any));
        let previous_claude = CLAUDE_OVERRIDE.with(|cell| cell.replace(claude));
        let previous_store = STORE.with(|cell| cell.replace(store));
        OverrideGuard {
            previous_any,
            previous_claude,
            previous_store,
        }
    }
}

#[cfg(test)]
pub(crate) fn with_reads_allowed(allowed: bool) -> test_backend::OverrideGuard {
    test_backend::with_state(Some(allowed), None, None)
}

#[cfg(test)]
pub(crate) fn with_claude_allowed(allowed: bool) -> test_backend::OverrideGuard {
    test_backend::with_state(None, Some(allowed), None)
}

#[cfg(test)]
pub(crate) fn with_mock_store(
    allowed: bool,
    entries: &[(&str, &str, &str)],
) -> test_backend::OverrideGuard {
    let store = entries
        .iter()
        .map(|(service, user, value)| {
            (
                (*service).to_string(),
                (*user).to_string(),
                (*value).to_string(),
            )
        })
        .map(|(service, user, value)| ((service, user), value))
        .collect();
    test_backend::with_state(Some(allowed), Some(allowed), Some(store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ProviderId;

    fn settings_with(disable: bool, avoid_claude: bool) -> Settings {
        let mut settings = Settings {
            disable_keychain_access: disable,
            ..Default::default()
        };
        settings.set_avoid_keychain_prompts(ProviderId::Claude, avoid_claude);
        settings
    }

    #[test]
    fn disable_keychain_access_blocks_every_scope() {
        let settings = settings_with(true, false);
        assert!(!allowed_for(&settings, Scope::Any));
        assert!(!allowed_for(&settings, Scope::Claude));
    }

    #[test]
    fn avoid_keychain_prompts_blocks_only_claude() {
        let settings = settings_with(false, true);
        assert!(allowed_for(&settings, Scope::Any));
        assert!(!allowed_for(&settings, Scope::Claude));
    }

    #[test]
    fn defaults_allow_keychain_access() {
        let settings = Settings::default();
        assert!(allowed_for(&settings, Scope::Any));
        assert!(allowed_for(&settings, Scope::Claude));
    }

    #[test]
    fn disabled_get_never_opens_the_os_keyring() {
        let _guard = with_reads_allowed(false);
        assert_eq!(
            get_password(Scope::Any, "ceiling-sbs-1023", "must-not-open"),
            Err(Error::Disabled)
        );
        assert_eq!(
            get_secret(Scope::Any, "ceiling-sbs-1023", "must-not-open"),
            None
        );
    }

    #[test]
    fn disabled_set_never_opens_the_os_keyring() {
        let _guard = with_reads_allowed(false);
        assert_eq!(
            set_password(Scope::Any, "ceiling-sbs-1023", "must-not-open", "secret"),
            Err(Error::Disabled)
        );
    }

    #[test]
    fn mock_store_is_ignored_when_access_is_disabled() {
        let _guard = with_mock_store(false, &[("svc", "api_key", "from-keyring")]);
        assert_eq!(get_secret(Scope::Any, "svc", "api_key"), None);
        assert_eq!(
            get_password(Scope::Any, "svc", "api_key"),
            Err(Error::Disabled)
        );
    }

    #[test]
    fn mock_store_returns_the_secret_when_access_is_allowed() {
        let _guard = with_mock_store(true, &[("svc", "api_key", "from-keyring")]);
        assert_eq!(
            get_secret(Scope::Any, "svc", "api_key").as_deref(),
            Some("from-keyring")
        );
    }

    #[test]
    fn claude_scope_stays_disabled_when_only_avoid_prompts_is_set() {
        let _guard = with_claude_allowed(false);
        assert_eq!(
            get_password(Scope::Claude, "Claude Code-credentials", "acct"),
            Err(Error::Disabled)
        );
    }

    /// Without keyring's `windows-native` feature, entries silently fall back
    /// to its in-memory mock store and never reach Credential Manager.
    #[cfg(windows)]
    #[test]
    fn os_entries_use_windows_credential_manager() {
        let entry = keyring::Entry::new("ceiling-backend-check", "probe").unwrap();
        assert!(
            entry
                .get_credential()
                .downcast_ref::<keyring::windows::WinCredential>()
                .is_some()
        );
    }
}
