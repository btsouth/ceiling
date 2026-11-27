//! Copilot Device Flow OAuth
//!
//! Implements GitHub's OAuth Device Flow for authenticating GitHub Copilot.
//! This allows the user to authorize the app via browser while the app polls for the token.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// VS Code's GitHub OAuth Client ID (public)
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const SCOPES: &str = "read:user";
/// Per-request deadline for the device-code and token-poll calls.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Device code response from GitHub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    /// The device verification code
    pub device_code: String,
    /// The user-visible code to enter
    pub user_code: String,
    /// The URL where the user should enter the code
    pub verification_uri: String,
    /// Pre-filled verification URL (includes user_code in query string)
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    /// How long until the codes expire (seconds)
    pub expires_in: u32,
    /// Polling interval in seconds
    pub interval: u32,
}

impl DeviceCodeResponse {
    /// Returns the best URL to open — prefers the pre-filled complete URI
    pub fn verification_url_to_open(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

/// Access token response from GitHub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessTokenResponse {
    /// The OAuth access token
    pub access_token: String,
    /// Token type (usually "bearer")
    pub token_type: String,
    /// Granted scopes
    pub scope: String,
}

/// Error response from GitHub OAuth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// Device flow errors
#[derive(Debug, Clone)]
pub enum DeviceFlowError {
    /// Network or HTTP error
    Network(String),
    /// Authorization is still pending (keep polling)
    AuthorizationPending,
    /// Need to slow down polling
    SlowDown,
    /// The device code expired
    ExpiredToken,
    /// Access was denied by the user
    AccessDenied,
    /// Generic OAuth error
    OAuthError(String),
    /// JSON parsing error
    ParseError(String),
}

impl std::fmt::Display for DeviceFlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceFlowError::Network(e) => write!(f, "Network error: {}", e),
            DeviceFlowError::AuthorizationPending => write!(f, "Authorization pending"),
            DeviceFlowError::SlowDown => write!(f, "Polling too fast, slowing down"),
            DeviceFlowError::ExpiredToken => write!(f, "Device code expired"),
            DeviceFlowError::AccessDenied => write!(f, "Access denied by user"),
            DeviceFlowError::OAuthError(e) => write!(f, "OAuth error: {}", e),
            DeviceFlowError::ParseError(e) => write!(f, "Parse error: {}", e),
        }
    }
}

impl std::error::Error for DeviceFlowError {}

/// Copilot Device Flow OAuth client
pub struct CopilotDeviceFlow {
    client: reqwest::Client,
}

impl CopilotDeviceFlow {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Request a device code from GitHub
    ///
    /// The returned response contains:
    /// - `user_code`: Display this to the user
    /// - `verification_uri`: User should visit this URL
    /// - `device_code`: Used for polling (don't show to user)
    /// - `interval`: How often to poll in seconds
    pub async fn request_device_code(&self) -> Result<DeviceCodeResponse, DeviceFlowError> {
        let mut params = HashMap::new();
        params.insert("client_id", CLIENT_ID);
        params.insert("scope", SCOPES);

        let response = self
            .client
            .post("https://github.com/login/device/code")
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await
            .map_err(|e| DeviceFlowError::Network(e.to_string()))?;

        if !response.status().is_success() {
            return Err(DeviceFlowError::Network(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let text = response
            .text()
            .await
            .map_err(|e| DeviceFlowError::Network(e.to_string()))?;

        serde_json::from_str(&text).map_err(|e| DeviceFlowError::ParseError(e.to_string()))
    }

    /// Poll for the access token
    ///
    /// Call this in a loop with the specified interval until you get a token or error.
    pub async fn poll_for_token(&self, device_code: &str) -> Result<String, DeviceFlowError> {
        let mut params = HashMap::new();
        params.insert("client_id", CLIENT_ID);
        params.insert("device_code", device_code);
        params.insert("grant_type", "urn:ietf:params:oauth:grant-type:device_code");

        let response = self
            .client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await
            .map_err(|e| DeviceFlowError::Network(e.to_string()))?;

        let text = response
            .text()
            .await
            .map_err(|e| DeviceFlowError::Network(e.to_string()))?;

        // First, try to parse as an error response
        if let Ok(error) = serde_json::from_str::<OAuthErrorResponse>(&text) {
            return match error.error.as_str() {
                "authorization_pending" => Err(DeviceFlowError::AuthorizationPending),
                "slow_down" => Err(DeviceFlowError::SlowDown),
                "expired_token" => Err(DeviceFlowError::ExpiredToken),
                "access_denied" => Err(DeviceFlowError::AccessDenied),
                _ => Err(DeviceFlowError::OAuthError(
                    error.error_description.unwrap_or(error.error),
                )),
            };
        }

        // Try to parse as success response
        let token_response: AccessTokenResponse =
            serde_json::from_str(&text).map_err(|e| DeviceFlowError::ParseError(e.to_string()))?;

        Ok(token_response.access_token)
    }

    /// Convenience method to run the full device flow
    ///
    /// Returns the device code response for displaying to user, and a future that
    /// will resolve to the access token when the user completes authorization.
    pub async fn start_flow(&self) -> Result<DeviceCodeResponse, DeviceFlowError> {
        self.request_device_code().await
    }

    /// Poll until token is obtained or flow fails
    ///
    /// Respects the interval and handles slow_down requests automatically.
    pub async fn wait_for_token(
        &self,
        device_code: &str,
        initial_interval: u32,
        expires_in: u32,
    ) -> Result<String, DeviceFlowError> {
        let mut interval = initial_interval;
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(u64::from(expires_in));

        loop {
            // Check if we've exceeded the timeout
            if start.elapsed() >= timeout {
                return Err(DeviceFlowError::ExpiredToken);
            }

            // Wait the required interval
            tokio::time::sleep(Duration::from_secs(u64::from(interval))).await;

            match self.poll_for_token(device_code).await {
                Ok(token) => return Ok(token),
                Err(DeviceFlowError::AuthorizationPending) => {
                    // Continue polling
                    continue;
                }
                Err(DeviceFlowError::SlowDown) => {
                    // Increase interval by 5 seconds as per OAuth spec
                    interval += 5;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Default for CopilotDeviceFlow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_flow_new() {
        let _flow = CopilotDeviceFlow::new();
    }

    #[test]
    fn test_error_display() {
        let err = DeviceFlowError::AuthorizationPending;
        assert_eq!(err.to_string(), "Authorization pending");

        let err = DeviceFlowError::ExpiredToken;
        assert_eq!(err.to_string(), "Device code expired");
    }
}
