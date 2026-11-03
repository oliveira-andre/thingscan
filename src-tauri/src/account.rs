//! The SmartThings account: the token, kept in `smartthings.json` in the app data folder
//! (readable only by this user), and the devices that token can see. Disconnecting keeps the
//! file with no token and the time, so account sync can tell a disconnect from never having
//! connected.
//!
//! The token is either a personal access token the user pasted, which lasts 24 hours, or an
//! OAuth grant from signing in through the user's own SmartThings app, which renews itself.
//! A refresh token works once and retires the access token before it, so every Mac and iPhone
//! shares the one grant through sync, and the newest wins. `smartthings-app.json` keeps the
//! app's client ID and secret, which SmartThings shows only once.

use crate::store::{StoreError, now};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use thingscan_core::cloud::{self, Client, CloudDevice, CloudError, Grant, OAuthApp};

const FILE: &str = "smartthings.json";
const APP_FILE: &str = "smartthings-app.json";
/// Renew once less than this is left of the access token's 24 hours.
const RENEW_BEFORE: u64 = 6 * 3600;
/// How often a running Mac looks at whether the token is due.
const CHECK: Duration = Duration::from_secs(10 * 60);

/// The token (none after a disconnect) and when it was saved or removed, in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    pub token: Option<String>,
    pub saved_at: u64,
    /// Set when the token renews itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthGrant>,
}

/// What renews an OAuth token: the app it was issued to and the refresh token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthGrant {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    /// When the access token stops working, in seconds.
    pub expires_at: u64,
}

impl OAuthGrant {
    fn app(&self) -> OAuthApp {
        OAuthApp { client_id: self.client_id.clone(), client_secret: self.client_secret.clone() }
    }
}

/// The SmartThings app to sign in through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedApp {
    #[serde(flatten)]
    pub app: OAuthApp,
    /// Thingscan made it, so sign-in asks for Thingscan's scopes; an app the user made offers
    /// its own.
    pub made_here: bool,
}

pub struct Account {
    path: PathBuf,
    app_path: PathBuf,
    token_url: String,
    token: Mutex<Option<TokenRecord>>,
    app: Mutex<Option<SavedApp>>,
    devices: Mutex<Vec<CloudDevice>>,
    /// One renewal at a time: a refresh token works once.
    renewing: tokio::sync::Mutex<()>,
    /// The refresh token SmartThings last refused, and why. It isn't tried again; a newer one
    /// from sync or a new sign-in replaces it.
    refused: Mutex<Option<(String, String)>>,
}

impl Account {
    pub fn open(dir: &Path) -> Self {
        Self::with_token_url(dir, cloud::TOKEN_URL)
    }

    fn with_token_url(dir: &Path, token_url: &str) -> Self {
        fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
            fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
        }
        let (path, app_path) = (dir.join(FILE), dir.join(APP_FILE));
        Self {
            token: Mutex::new(read(&path)),
            app: Mutex::new(read(&app_path)),
            path,
            app_path,
            token_url: token_url.to_string(),
            devices: Mutex::new(Vec::new()),
            renewing: tokio::sync::Mutex::new(()),
            refused: Mutex::new(None),
        }
    }

    pub fn client(&self) -> Option<Client> {
        self.token().map(|t| Client::new(&t))
    }

    /// The whole token, for the account menu's reveal and copy buttons.
    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap().as_ref().and_then(|t| t.token.clone())
    }

    /// The last four characters, enough to recognise the token without showing it.
    pub fn token_hint(&self) -> Option<String> {
        let chars: Vec<char> = self.token()?.chars().collect();
        Some(chars[chars.len().saturating_sub(4)..].iter().collect())
    }

    /// When the token was saved or last renewed, in seconds since the epoch.
    pub fn saved_at(&self) -> Option<u64> {
        self.token.lock().unwrap().as_ref().filter(|t| t.token.is_some()).map(|t| t.saved_at)
    }

    /// The token or its removal, for account sync.
    pub fn record(&self) -> Option<TokenRecord> {
        self.token.lock().unwrap().clone()
    }

    fn grant(&self) -> Option<OAuthGrant> {
        self.token.lock().unwrap().as_ref().filter(|t| t.token.is_some()).and_then(|t| t.oauth.clone())
    }

    /// The token renews itself.
    pub fn renews(&self) -> bool {
        self.grant().is_some()
    }

    /// When a renewing token's access token runs out, in seconds.
    pub fn expires_at(&self) -> Option<u64> {
        self.grant().map(|g| g.expires_at)
    }

    /// Why the token stopped renewing, while it's still the one SmartThings refused.
    pub fn renew_error(&self) -> Option<String> {
        let grant = self.grant()?;
        let refused = self.refused.lock().unwrap();
        refused.as_ref().filter(|(token, _)| *token == grant.refresh_token).map(|(_, why)| why.clone())
    }

    /// Keeps a personal access token.
    pub fn save(&self, token: &str) -> Result<(), StoreError> {
        self.write(TokenRecord { token: Some(token.trim().to_string()), saved_at: now(), oauth: None })
    }

    /// Keeps a fresh pair from SmartThings' token endpoint.
    pub fn save_grant(&self, app: &OAuthApp, grant: Grant) -> Result<(), StoreError> {
        let at = now();
        self.write(TokenRecord {
            token: Some(grant.access_token),
            saved_at: at,
            oauth: Some(OAuthGrant {
                client_id: app.client_id.clone(),
                client_secret: app.client_secret.clone(),
                refresh_token: grant.refresh_token,
                expires_at: at + grant.expires_in,
            }),
        })
    }

    /// Takes the token (or its removal) that account sync brought from another device.
    pub fn apply(&self, record: TokenRecord) -> Result<(), StoreError> {
        if record.token.is_none() {
            self.devices.lock().unwrap().clear();
        }
        self.write(record)
    }

    pub fn forget(&self) -> Result<(), StoreError> {
        self.devices.lock().unwrap().clear();
        self.write(TokenRecord { token: None, saved_at: now(), oauth: None })
    }

    fn write(&self, record: TokenRecord) -> Result<(), StoreError> {
        // In memory first: after a renewal the old refresh token is spent, even if the disk is not.
        *self.token.lock().unwrap() = Some(record.clone());
        save_private(&self.path, &record)
    }

    /// The app to sign in through: the one saved here, or the one the current token came from.
    pub fn app(&self) -> Option<SavedApp> {
        self.app.lock().unwrap().clone().or_else(|| Some(SavedApp { app: self.grant()?.app(), made_here: false }))
    }

    pub fn save_app(&self, app: OAuthApp, made_here: bool) -> Result<(), StoreError> {
        let saved = SavedApp { app, made_here };
        *self.app.lock().unwrap() = Some(saved.clone());
        save_private(&self.app_path, &saved)
    }

    /// Renews a renewing token when less than six hours of it are left, or when `failed` (a
    /// token SmartThings just turned down) is still the current one. True when there's now a
    /// token other than `failed` to try.
    pub async fn renew(&self, failed: Option<&str>) -> Result<bool, CloudError> {
        let _one = self.renewing.lock().await;
        let (Some(token), Some(grant)) = (self.token(), self.grant()) else { return Ok(false) };
        match failed {
            Some(failed) if failed != token => return Ok(true),
            None if grant.expires_at > now() + RENEW_BEFORE => return Ok(false),
            _ => {}
        }
        if let Some(why) = self.renew_error() {
            return Err(CloudError::Grant(why));
        }
        match cloud::refresh_at(&self.token_url, &grant.app(), &grant.refresh_token).await {
            Ok(fresh) => {
                self.save_grant(&grant.app(), fresh).map_err(|e| CloudError::Api {
                    status: 0,
                    message: format!("couldn't save the renewed token: {e}"),
                })?;
                Ok(true)
            }
            Err(CloudError::Grant(reason)) => {
                let why = format!(
                    "SmartThings stopped renewing Thingscan's access ({reason}). Another device may have renewed it and not synced yet; otherwise sign in to SmartThings again."
                );
                *self.refused.lock().unwrap() = Some((grant.refresh_token, why.clone()));
                Err(CloudError::Grant(why))
            }
            Err(e) => Err(e),
        }
    }

    pub fn devices(&self) -> Vec<CloudDevice> {
        self.devices.lock().unwrap().clone()
    }

    pub fn set_devices(&self, devices: Vec<CloudDevice>) {
        *self.devices.lock().unwrap() = devices;
    }

    pub fn device(&self, id: &str) -> Option<CloudDevice> {
        self.devices.lock().unwrap().iter().find(|d| d.device_id == id).cloned()
    }
}

/// Renews the token in the background while Thingscan runs, so it never lapses and the
/// iPhone gets a working one through sync.
pub fn start<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let mut last_error = None;
        loop {
            let result = app.state::<Account>().renew(None).await;
            let error = match &result {
                Err(CloudError::Grant(why)) => Some(why.clone()),
                _ => None,
            };
            if matches!(result, Ok(true)) || error != last_error {
                let _ = app.emit("smartthings-changed", ());
            }
            last_error = error;
            tokio::time::sleep(CHECK).await;
        }
    });
}

fn save_private<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Io(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &json).map_err(|e| StoreError::Io(format!("can't write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(|e| StoreError::Io(format!("can't save {}: {e}", path.display())))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Form, Router};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn saves_privately_reloads_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::open(dir.path());
        assert!(account.client().is_none());
        account.save("  abc-123 \n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let reopened = Account::open(dir.path());
        assert!(reopened.client().is_some());
        assert_eq!(reopened.token().as_deref(), Some("abc-123"));
        assert_eq!(reopened.token_hint().as_deref(), Some("-123"));
        assert!(reopened.saved_at().is_some());
        assert!(!reopened.renews());
        reopened.forget().unwrap();
        let forgotten = Account::open(dir.path());
        assert!(forgotten.client().is_none());
        assert!(forgotten.saved_at().is_none());
        // The disconnect itself is remembered, with its time, for account sync.
        assert!(forgotten.record().is_some_and(|r| r.token.is_none() && r.saved_at > 0));
    }

    #[test]
    fn reads_files_from_before_disconnects_were_kept() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE), r#"{"token":"abc-123","savedAt":5}"#).unwrap();
        let account = Account::open(dir.path());
        assert_eq!(account.token().as_deref(), Some("abc-123"));
        assert_eq!(account.record(), Some(TokenRecord { token: Some("abc-123".into()), saved_at: 5, oauth: None }));
    }

    #[test]
    fn keeps_the_app_and_the_grant_across_launches() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::open(dir.path());
        assert!(account.app().is_none());
        let app = OAuthApp { client_id: "c".into(), client_secret: "s".into() };
        let grant = Grant { access_token: "a-1".into(), refresh_token: "r-1".into(), expires_in: 86_399 };
        account.save_grant(&app, grant).unwrap();
        // Signed in through an app that came by sync: sign in again through the same one.
        assert_eq!(account.app(), Some(SavedApp { app: app.clone(), made_here: false }));
        account.save_app(app.clone(), true).unwrap();
        let reopened = Account::open(dir.path());
        assert_eq!(reopened.app(), Some(SavedApp { app, made_here: true }));
        assert!(reopened.renews());
        assert_eq!(reopened.token().as_deref(), Some("a-1"));
        let expires = reopened.expires_at().unwrap();
        assert!(expires > now() + 86_000 && expires <= now() + 86_399);
        let json = fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(json.contains("\"refreshToken\": \"r-1\""), "{json}");
    }

    /// A token endpoint whose refresh tokens work once, like SmartThings'.
    async fn token_server() -> (String, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let issued = Arc::new(Mutex::new(vec!["r-1".to_string()]));
        let counter = calls.clone();
        let router = Router::new().route(
            "/oauth/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let (calls, issued) = (counter.clone(), issued.clone());
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut issued = issued.lock().unwrap();
                    let given = form.get("refresh_token").cloned().unwrap_or_default();
                    if form.get("grant_type").map(String::as_str) != Some("refresh_token") || !issued.contains(&given) {
                        let body = json!({ "error": "invalid_grant", "error_description": "Invalid refresh token" });
                        return (axum::http::StatusCode::BAD_REQUEST, axum::Json(body));
                    }
                    issued.retain(|t| *t != given);
                    issued.push(format!("r-{}", n + 1));
                    let body = json!({ "access_token": format!("a-{}", n + 1), "refresh_token": format!("r-{}", n + 1), "expires_in": 86399 });
                    (axum::http::StatusCode::OK, axum::Json(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, router).into_future());
        (url, calls)
    }

    #[tokio::test]
    async fn renews_when_due_and_once_per_refused_token() {
        let (url, calls) = token_server().await;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::with_token_url(dir.path(), &url);
        let app = OAuthApp { client_id: "c".into(), client_secret: "s".into() };

        // A personal token never renews.
        account.save("pat").unwrap();
        assert_eq!(account.renew(None).await, Ok(false));
        assert_eq!(account.renew(Some("pat")).await, Ok(false));

        // Plenty of time left: nothing to do.
        account
            .save_grant(&app, Grant { access_token: "a-1".into(), refresh_token: "r-1".into(), expires_in: 86_399 })
            .unwrap();
        assert_eq!(account.renew(None).await, Ok(false));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // SmartThings turned the access token down: renew now.
        assert_eq!(account.renew(Some("a-1")).await, Ok(true));
        assert_eq!(account.token().as_deref(), Some("a-2"));
        // A call that failed with the old token just retries with the new one.
        assert_eq!(account.renew(Some("a-1")).await, Ok(true));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Less than six hours left: renew by itself.
        account
            .save_grant(&app, Grant { access_token: "a-2".into(), refresh_token: "r-2".into(), expires_in: 3600 })
            .unwrap();
        assert_eq!(account.renew(None).await, Ok(true));
        assert_eq!(account.token().as_deref(), Some("a-3"));
        assert!(account.renew_error().is_none());

        // Another device spent this refresh token: refused once, and not tried again.
        account
            .save_grant(&app, Grant { access_token: "a-x".into(), refresh_token: "r-2".into(), expires_in: 60 })
            .unwrap();
        assert!(matches!(account.renew(None).await, Err(CloudError::Grant(_))));
        assert!(matches!(account.renew(None).await, Err(CloudError::Grant(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(account.renew_error().is_some());
        assert!(account.renews(), "the grant stays until sync or a sign-in replaces it");

        // Sync brings the grant the other device renewed.
        let record = TokenRecord {
            token: Some("a-3".into()),
            saved_at: now() + 1,
            oauth: Some(OAuthGrant {
                client_id: "c".into(),
                client_secret: "s".into(),
                refresh_token: "r-3".into(),
                expires_at: now() + 60,
            }),
        };
        account.apply(record).unwrap();
        assert!(account.renew_error().is_none());
        assert_eq!(account.renew(None).await, Ok(true));
        assert_eq!(account.token().as_deref(), Some("a-5"));
    }
}
