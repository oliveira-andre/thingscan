//! The Thingscan account: signing in to a Thingscan server (`crates/thingscan-server`) and
//! keeping this Mac's devices and SmartThings token in sync with the user's other Macs and
//! iPhones, through a vault the server can't read. TV pairing tokens stay out of it: a Samsung TV
//! only honours a token on the device it gave it to, so each Mac and iPhone keeps its own.
//!
//! Keys, all derived on the device (the iPhone app derives the same, in `AccountSync.swift`):
//! - `master = PBKDF2-HMAC-SHA256(password, SHA-256("thingscan-account-v1:" + email), 600 000)`
//! - `authKey = HKDF-SHA256(master, info "thingscan-auth")` is all the server sees of the password.
//! - `wrapKey = HKDF-SHA256(master, info "thingscan-wrap")` seals a random vault key (associated
//!   data "thingscan-vault-key"); the server keeps that sealed copy for the next sign-in.
//! - The vault key seals the vault JSON with AES-256-GCM, associated data
//!   "thingscan-vault:" + user id.
//!
//! A sync reads the vault, merges it with what's here (`merge`), saves the result here and, if
//! that adds anything, writes it back based on the version it read. A refused write means
//! another device wrote first; read again and merge with that.

use crate::account::{Account, TokenRecord};
use crate::store::{Removed, SavedDevice, Store, now};
use crate::sync::{open, seal};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hkdf::Hkdf;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use thingscan_core::Endpoint;

const FILE: &str = "thingscan-account.json";
pub const ITERATIONS: u32 = 600_000;
/// The vault layout this version writes. A newer one is read but never overwritten.
/// 2: the SmartThings token may carry an OAuth grant, which older versions would drop.
pub const FORMAT: u32 = 2;
/// Removals are forgotten after this long; every device will have synced by then.
const REMOVED_KEEP: u64 = 90 * 24 * 3600;
const TICK: Duration = Duration::from_secs(2);
/// How often to look for changes from other devices.
const POLL: Duration = Duration::from_secs(60);
const MIN_PASSWORD: usize = 8;
const KEY_AAD: &[u8] = b"thingscan-vault-key";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    server: String,
    email: String,
    user_id: String,
    token: String,
    /// Base64 of the 32-byte vault key. Never sent anywhere.
    vault_key: String,
}

/// What the vault holds, once opened.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultDoc {
    pub format: u32,
    #[serde(default)]
    pub devices: Vec<SavedDevice>,
    #[serde(default)]
    pub removed: Vec<Removed>,
    #[serde(default)]
    pub smartthings: Option<TokenRecord>,
}

#[derive(Default)]
struct SyncState {
    last_sync: Option<u64>,
    error: Option<String>,
    syncing: bool,
    /// The last vault read or written, so an unchanged one needn't be downloaded again.
    remote: Option<(u64, VaultDoc)>,
}

pub struct Vault {
    path: PathBuf,
    session: Mutex<Option<Session>>,
    state: Mutex<SyncState>,
    /// One sync at a time.
    running: tokio::sync::Mutex<()>,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    signed_in: bool,
    email: Option<String>,
    server: Option<String>,
    /// Seconds since the epoch.
    last_sync: Option<u64>,
    syncing: bool,
    error: Option<String>,
}

#[derive(Debug)]
pub struct VaultError(pub String);

fn fail<T>(message: impl Into<String>) -> Result<T, VaultError> {
    Err(VaultError(message.into()))
}

pub struct Keys {
    pub auth: [u8; 32],
    pub wrap: [u8; 32],
}

/// Stretches the password; takes a fraction of a second on purpose.
pub fn derive_keys(email: &str, password: &str) -> Keys {
    let salt = Sha256::digest(format!("thingscan-account-v1:{email}").as_bytes());
    let mut master = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, ITERATIONS, &mut master);
    let hkdf = Hkdf::<Sha256>::new(Some(&[]), &master);
    let (mut auth, mut wrap) = ([0u8; 32], [0u8; 32]);
    hkdf.expand(b"thingscan-auth", &mut auth).expect("32 bytes is a valid HKDF length");
    hkdf.expand(b"thingscan-wrap", &mut wrap).expect("32 bytes is a valid HKDF length");
    Keys { auth, wrap }
}

/// "sync.example.com" → "https://sync.example.com"; an explicit http:// is kept for testing on a LAN.
fn normalize_server(text: &str) -> Result<String, VaultError> {
    let text = text.trim().trim_end_matches('/');
    let full = if text.contains("://") { text.to_string() } else { format!("https://{text}") };
    match reqwest::Url::parse(&full) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => Ok(full),
        _ => fail("Enter the server's address, e.g. sync.example.com."),
    }
}

impl Vault {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let session = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("HTTP client builds");
        Self { path, session: Mutex::new(session), state: Mutex::default(), running: Default::default(), http }
    }

    pub fn signed_in(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    pub fn status(&self) -> AccountStatus {
        let session = self.session.lock().unwrap();
        let state = self.state.lock().unwrap();
        AccountStatus {
            signed_in: session.is_some(),
            email: session.as_ref().map(|s| s.email.clone()),
            server: session.as_ref().map(|s| s.server.clone()),
            last_sync: state.last_sync,
            syncing: state.syncing,
            error: state.error.clone(),
        }
    }

    pub async fn sign_up<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server: &str,
        email: &str,
        password: &str,
    ) -> Result<AccountStatus, VaultError> {
        self.begin(app, server, email, password, true).await
    }

    pub async fn sign_in<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server: &str,
        email: &str,
        password: &str,
    ) -> Result<AccountStatus, VaultError> {
        self.begin(app, server, email, password, false).await
    }

    async fn begin<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        server: &str,
        email: &str,
        password: &str,
        create: bool,
    ) -> Result<AccountStatus, VaultError> {
        let server = normalize_server(server)?;
        let email = email.trim().to_lowercase();
        if !email.contains('@') {
            return fail("Enter your email address.");
        }
        if password.is_empty() {
            return fail("Enter your password.");
        }
        if create && password.chars().count() < MIN_PASSWORD {
            return fail(format!("Use at least {MIN_PASSWORD} characters for the password."));
        }
        let (e, p) = (email.clone(), password.to_string());
        let keys = tauri::async_runtime::spawn_blocking(move || derive_keys(&e, &p))
            .await
            .map_err(|_| VaultError("Couldn't derive the keys.".into()))?;
        let device = crate::sync::mac_name();
        let body = json!({ "email": email, "authKey": B64.encode(keys.auth), "device": device });
        let session = if create {
            let mut vault_key = [0u8; 32];
            getrandom::fill(&mut vault_key).map_err(|_| VaultError("No randomness.".into()))?;
            let wrapped = seal(&keys.wrap, KEY_AAD, &vault_key).map_err(|m| VaultError(m.into()))?;
            let mut body = body;
            body["wrappedKey"] = json!(B64.encode(wrapped));
            let (status, reply) = self.call(Method::POST, &server, "/v1/signup", None, Some(body)).await?;
            if status != 201 {
                return Err(api_error(status, &reply));
            }
            session_from(&server, &reply, B64.encode(vault_key))?
        } else {
            let (status, reply) = self.call(Method::POST, &server, "/v1/login", None, Some(body)).await?;
            if status != 200 {
                return Err(api_error(status, &reply));
            }
            let vault_key = reply["wrappedKey"]
                .as_str()
                .and_then(|w| B64.decode(w).ok())
                .and_then(|w| open(&keys.wrap, KEY_AAD, &w))
                .filter(|k| k.len() == 32)
                .ok_or_else(|| VaultError("This account's key couldn't be opened.".into()))?;
            session_from(&server, &reply, B64.encode(vault_key))?
        };
        self.save_session(Some(session))?;
        *self.state.lock().unwrap() = SyncState::default();
        Ok(self.sync(app).await)
    }

    /// Signs out here. The devices stay on this Mac; they just stop syncing.
    pub async fn sign_out<R: Runtime>(&self, app: &AppHandle<R>) -> AccountStatus {
        let session = self.session.lock().unwrap().clone();
        let _ = self.save_session(None);
        *self.state.lock().unwrap() = SyncState::default();
        if let Some(session) = session {
            // Best effort: the session ends on the server too if it's reachable.
            let _ = self.call(Method::POST, &session.server, "/v1/logout", Some(&session.token), None).await;
        }
        let status = self.status();
        let _ = app.emit("account-changed", &status);
        status
    }

    /// Syncs now and reports how it went (errors end up in the status).
    pub async fn sync<R: Runtime>(&self, app: &AppHandle<R>) -> AccountStatus {
        let _running = self.running.lock().await;
        if !self.signed_in() {
            return self.status();
        }
        self.state.lock().unwrap().syncing = true;
        let _ = app.emit("account-changed", self.status());
        let result = self.sync_rounds(app).await;
        {
            let mut state = self.state.lock().unwrap();
            state.syncing = false;
            match result {
                Ok(()) => {
                    state.last_sync = Some(now());
                    state.error = None;
                }
                Err(e) => state.error = Some(e.0),
            }
        }
        let status = self.status();
        let _ = app.emit("account-changed", &status);
        status
    }

    async fn sync_rounds<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), VaultError> {
        for _ in 0..4 {
            let Some(session) = self.session.lock().unwrap().clone() else { return fail("Signed out.") };
            let key: [u8; 32] = B64
                .decode(&session.vault_key)
                .ok()
                .and_then(|k| k.try_into().ok())
                .ok_or_else(|| VaultError("The saved vault key is damaged. Sign in again.".into()))?;
            let cached = self.state.lock().unwrap().remote.clone();
            let path = cached.as_ref().map_or("/v1/vault".to_string(), |(v, _)| format!("/v1/vault?known={v}"));
            let (status, reply) = self.call(Method::GET, &session.server, &path, Some(&session.token), None).await?;
            if status == 401 {
                let _ = self.save_session(None);
                return fail("Signed out by the server. Sign in again.");
            }
            if status != 200 {
                return Err(api_error(status, &reply));
            }
            let version = reply["version"].as_u64().unwrap_or(0);
            let remote = match cached {
                Some((v, doc)) if reply["unchanged"] == true && v == version => doc,
                _ => without_tv_tokens(decode(&key, &session.user_id, &reply["vault"])?),
            };
            self.state.lock().unwrap().remote = Some((version, remote.clone()));
            if self.merge_and_push(app, &session, &key, version, remote).await? {
                return Ok(());
            }
        }
        fail("Other devices kept changing your account; try again.")
    }

    /// Returns false when it has to start over: this Mac's devices changed while merging, or
    /// another device wrote first.
    async fn merge_and_push<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        session: &Session,
        key: &[u8; 32],
        version: u64,
        remote: VaultDoc,
    ) -> Result<bool, VaultError> {
        let store = app.state::<Store>();
        let account = app.state::<Account>();
        let (generation, devices, removed) = store.snapshot();
        let tv_tokens: BTreeMap<String, String> =
            devices.iter().filter_map(|d| Some((d.id.clone(), d.tv_token.clone()?))).collect();
        let local = without_tv_tokens(VaultDoc { format: FORMAT, devices, removed, smartthings: account.record() });
        let merged = merge(&local, &remote);
        // This Mac's own TV pairings go back on its devices.
        let mut here = merged.devices.clone();
        for device in &mut here {
            device.tv_token = tv_tokens.get(&device.id).cloned();
        }
        match store.replace(generation, here, merged.removed.clone()) {
            Ok(None) => return Ok(false),
            Ok(Some(true)) => {
                let _ = app.emit("devices-changed", ());
            }
            Ok(Some(false)) => {}
            Err(e) => return fail(e.to_string()),
        }
        if merged.smartthings != local.smartthings
            && let Some(record) = merged.smartthings.clone()
        {
            account.apply(record).map_err(|e| VaultError(e.to_string()))?;
            let _ = app.emit("smartthings-changed", ());
        }
        if same_for_vault(&merged, &remote) {
            return Ok(true);
        }
        let plain = serde_json::to_vec(&merged).map_err(|e| VaultError(e.to_string()))?;
        let sealed = seal(key, vault_aad(&session.user_id).as_bytes(), &plain).map_err(|m| VaultError(m.into()))?;
        let body = json!({ "baseVersion": version, "vault": B64.encode(sealed) });
        let (status, reply) =
            self.call(Method::PUT, &session.server, "/v1/vault", Some(&session.token), Some(body)).await?;
        match status {
            200 => {
                let written = reply["version"].as_u64().unwrap_or(version + 1);
                self.state.lock().unwrap().remote = Some((written, merged));
                Ok(true)
            }
            409 => {
                self.state.lock().unwrap().remote = None;
                Ok(false)
            }
            _ => Err(api_error(status, &reply)),
        }
    }

    async fn call(
        &self,
        method: Method,
        server: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(u16, Value), VaultError> {
        let mut request = self.http.request(method, format!("{server}{path}"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let host = reqwest::Url::parse(server).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        let response = request.send().await.map_err(|_| VaultError(format!("Couldn't reach {host}.")))?;
        let status = response.status().as_u16();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    /// The session holds the vault key, so the file is readable only by this user.
    fn save_session(&self, session: Option<Session>) -> Result<(), VaultError> {
        let io = |e: std::io::Error| VaultError(format!("Couldn't save the account: {e}"));
        match &session {
            Some(s) => {
                let json = serde_json::to_vec_pretty(s).map_err(|e| VaultError(e.to_string()))?;
                let tmp = self.path.with_extension("json.tmp");
                write_private(&tmp, &json).map_err(io)?;
                fs::rename(&tmp, &self.path).map_err(io)?;
            }
            None => match fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(e)),
                _ => {}
            },
        }
        *self.session.lock().unwrap() = session;
        Ok(())
    }
}

fn session_from(server: &str, reply: &Value, vault_key: String) -> Result<Session, VaultError> {
    let text = |key: &str| reply[key].as_str().map(String::from);
    match (text("userId"), text("email"), text("token")) {
        (Some(user_id), Some(email), Some(token)) => {
            Ok(Session { server: server.to_string(), email, user_id, token, vault_key })
        }
        _ => fail("The server's reply didn't make sense. Is that a Thingscan server?"),
    }
}

fn api_error(status: u16, reply: &Value) -> VaultError {
    VaultError(reply["error"].as_str().map(String::from).unwrap_or_else(|| {
        if status == 404 {
            "That address isn't a Thingscan server.".into()
        } else {
            format!("The server answered {status}.")
        }
    }))
}

fn vault_aad(user_id: &str) -> String {
    format!("thingscan-vault:{user_id}")
}

/// Opens the vault the server sent; none yet is an empty one.
fn decode(key: &[u8; 32], user_id: &str, value: &Value) -> Result<VaultDoc, VaultError> {
    let Some(text) = value.as_str() else { return Ok(VaultDoc::default()) };
    let plain = B64
        .decode(text)
        .ok()
        .and_then(|sealed| open(key, vault_aad(user_id).as_bytes(), &sealed))
        .ok_or_else(|| VaultError("Your account's data couldn't be opened. Sign out and in again.".into()))?;
    let doc: VaultDoc = serde_json::from_slice(&plain)
        .map_err(|e| VaultError(format!("Your account's data couldn't be read ({e}). Update Thingscan.")))?;
    if doc.format > FORMAT {
        return fail("A newer Thingscan saved your account's data. Update Thingscan on this Mac.");
    }
    Ok(doc)
}

fn without_tv_tokens(mut doc: VaultDoc) -> VaultDoc {
    doc.devices.iter_mut().for_each(|d| d.tv_token = None);
    doc
}

/// Equal apart from when devices were last seen, which changes every check and isn't worth a write.
fn same_for_vault(a: &VaultDoc, b: &VaultDoc) -> bool {
    let unseen = |doc: &VaultDoc| {
        let mut doc = doc.clone();
        doc.devices.iter_mut().for_each(|d| d.last_seen = None);
        doc
    };
    unseen(a) == unseen(b)
}

/// Combines this device's state with the vault's. `remote` wins ties, so devices that merge
/// the same inputs agree. The iPhone app's `VaultMerge` follows the same rules.
/// - A device's name, room and SmartThings link come from its newest edit.
/// - What was observed (addresses, endpoints, details) comes from whichever saw it last.
/// - A removal wins over edits made before it.
/// - The SmartThings token (or its removal) with the newest time wins. A renewal counts as a
///   change, and only the newest renewal's refresh token still works.
pub fn merge(local: &VaultDoc, remote: &VaultDoc) -> VaultDoc {
    let mut removed: BTreeMap<&str, u64> = BTreeMap::new();
    for r in local.removed.iter().chain(&remote.removed) {
        let at = removed.entry(r.id.as_str()).or_insert(0);
        *at = (*at).max(r.removed_at);
    }
    let mut ids: Vec<&str> = remote.devices.iter().map(|d| d.id.as_str()).collect();
    ids.extend(local.devices.iter().map(|d| d.id.as_str()).filter(|id| !remote.devices.iter().any(|d| d.id == *id)));

    let mut devices = Vec::new();
    for id in ids {
        let mine = local.devices.iter().find(|d| d.id == id);
        let theirs = remote.devices.iter().find(|d| d.id == id);
        let device = match (mine, theirs) {
            (Some(mine), Some(theirs)) => merge_device(mine, theirs),
            (Some(only), None) | (None, Some(only)) => only.clone(),
            (None, None) => continue,
        };
        match removed.get(id) {
            Some(&at) if at >= device.edited_at => continue,
            // Edited again after it was removed somewhere: keep it.
            Some(_) => {
                removed.remove(id);
            }
            None => {}
        }
        devices.push(device);
    }
    let cutoff = now().saturating_sub(REMOVED_KEEP);
    let removed = removed
        .into_iter()
        .filter(|(_, at)| *at >= cutoff)
        .map(|(id, removed_at)| Removed { id: id.to_string(), removed_at })
        .collect();
    let smartthings = match (&local.smartthings, &remote.smartthings) {
        (Some(mine), Some(theirs)) => Some(if mine.saved_at > theirs.saved_at { mine } else { theirs }.clone()),
        (mine, theirs) => theirs.clone().or_else(|| mine.clone()),
    };
    VaultDoc { format: FORMAT, devices, removed, smartthings }
}

fn merge_device(mine: &SavedDevice, theirs: &SavedDevice) -> SavedDevice {
    let edits = if mine.edited_at > theirs.edited_at { mine } else { theirs };
    let (seen, other) =
        if mine.last_seen.unwrap_or(0) > theirs.last_seen.unwrap_or(0) { (mine, theirs) } else { (theirs, mine) };
    let mut out = seen.clone();
    out.name = edits.name.clone();
    out.room = edits.room.clone();
    out.manufacturer = seen.manufacturer.clone().or_else(|| other.manufacturer.clone());
    out.model = seen.model.clone().or_else(|| other.model.clone());
    for id in &other.ids {
        if !out.ids.contains(id) {
            out.ids.push(id.clone());
        }
    }
    out.sources.extend(other.sources.iter().copied());
    for (key, value) in &other.details {
        out.details.entry(key.clone()).or_insert_with(|| value.clone());
    }
    // The SmartThings link follows the newest edit; its id and endpoint go last.
    out.ids.retain(|id| !id.starts_with("st:"));
    out.endpoints.retain(|e| !matches!(e, Endpoint::Cloud { .. }));
    if let Some(cloud_id) = &edits.cloud_id {
        out.ids.push(format!("st:{cloud_id}"));
        out.endpoints.push(Endpoint::Cloud { device_id: cloud_id.clone() });
    }
    out.cloud_id = edits.cloud_id.clone();
    out.last_seen = mine.last_seen.max(theirs.last_seen);
    out.added_at = [mine.added_at, theirs.added_at].into_iter().filter(|t| *t > 0).min().unwrap_or(0);
    out.edited_at = mine.edited_at.max(theirs.edited_at);
    out
}

/// Syncs in the background while signed in: soon after anything changes here, and every
/// minute to pick up changes from other devices.
pub fn start<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let mut seen: Option<(u64, Option<TokenRecord>)> = None;
        let mut last = Instant::now();
        loop {
            let vault = app.state::<Vault>();
            if vault.signed_in() {
                let current = (app.state::<Store>().generation(), app.state::<Account>().record());
                if seen.as_ref() != Some(&current) || last.elapsed() >= POLL {
                    vault.sync(&app).await;
                    seen = Some((app.state::<Store>().generation(), app.state::<Account>().record()));
                    last = Instant::now();
                }
            } else {
                seen = None;
            }
            tokio::time::sleep(TICK).await;
        }
    });
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
    use crate::account::OAuthGrant;
    use crate::store::tests::scanned;
    use std::sync::Arc;

    fn device(id: &str, name: &str, edited_at: u64, last_seen: Option<u64>) -> SavedDevice {
        let mut d = SavedDevice {
            id: id.into(),
            name: name.into(),
            room: None,
            kind: thingscan_core::Kind::Tv,
            manufacturer: None,
            model: None,
            ids: vec![format!("upnp:{id}")],
            endpoints: vec![],
            sources: Default::default(),
            details: Default::default(),
            added_at: 100,
            last_seen,
            last_address: None,
            cloud_id: None,
            tv_token: None,
            edited_at,
        };
        d.last_address = last_seen.map(|t| format!("10.0.0.{}", t % 250));
        d
    }

    fn doc(devices: Vec<SavedDevice>, removed: Vec<Removed>) -> VaultDoc {
        VaultDoc { format: FORMAT, devices, removed, smartthings: None }
    }

    #[test]
    fn newest_edit_wins_and_the_newest_sighting_supplies_the_facts() {
        let mut mine = device("tv", "TV", 200, Some(500));
        mine.room = Some("Den".into());
        let mut theirs = device("tv", "Living room TV", 300, Some(400));
        theirs.ids.push("ocf:abc".into());
        let merged = merge(&doc(vec![mine], vec![]), &doc(vec![theirs], vec![]));
        let tv = &merged.devices[0];
        assert_eq!((tv.name.as_str(), tv.room.as_deref()), ("Living room TV", None));
        assert_eq!(tv.last_address.as_deref(), Some("10.0.0.0"), "the newer sighting's address");
        assert_eq!(tv.ids, vec!["upnp:tv".to_string(), "ocf:abc".to_string()]);
        assert_eq!((tv.last_seen, tv.edited_at), (Some(500), 300));
        // Merging again changes nothing.
        assert_eq!(merge(&merged, &merged), merged);
    }

    #[test]
    fn removals_beat_older_edits_but_not_newer_ones() {
        let now = now();
        let kept = device("a", "A", now, None);
        let gone = device("b", "B", now - 50, None);
        let remote = doc(vec![kept.clone(), gone], vec![]);
        let local = doc(
            vec![],
            vec![Removed { id: "a".into(), removed_at: now - 10 }, Removed { id: "b".into(), removed_at: now - 10 }],
        );
        let merged = merge(&local, &remote);
        assert_eq!(merged.devices, vec![kept]);
        assert_eq!(merged.removed, vec![Removed { id: "b".into(), removed_at: now - 10 }]);
        // Long-forgotten removals are dropped.
        let old = doc(vec![], vec![Removed { id: "z".into(), removed_at: 5 }]);
        assert!(merge(&old, &doc(vec![], vec![])).removed.is_empty());
    }

    #[test]
    fn the_smartthings_link_and_token_follow_the_newest_change() {
        let mut linked = device("ac", "A/C", 300, None);
        linked.cloud_id = Some("c1".into());
        linked.ids.push("st:c1".into());
        linked.endpoints.push(Endpoint::Cloud { device_id: "c1".into() });
        let unlinked = device("ac", "A/C", 200, None);
        let local = VaultDoc {
            smartthings: Some(TokenRecord { token: None, saved_at: 50, oauth: None }),
            ..doc(vec![unlinked], vec![])
        };
        let remote = VaultDoc {
            smartthings: Some(TokenRecord { token: Some("t".into()), saved_at: 40, oauth: None }),
            ..doc(vec![linked.clone()], vec![])
        };
        let merged = merge(&local, &remote);
        assert_eq!(merged.devices[0].cloud_id.as_deref(), Some("c1"));
        assert_eq!(merged.devices[0].ids, linked.ids);
        assert_eq!(merged.devices[0].endpoints, linked.endpoints);
        assert_eq!(
            merged.smartthings,
            Some(TokenRecord { token: None, saved_at: 50, oauth: None }),
            "the later disconnect wins"
        );
    }

    #[test]
    fn the_newest_renewal_wins_with_its_refresh_token() {
        let grant = |refresh: &str| OAuthGrant {
            client_id: "c".into(),
            client_secret: "s".into(),
            refresh_token: refresh.into(),
            expires_at: 1000,
        };
        let renewed = TokenRecord { token: Some("a-2".into()), saved_at: 70, oauth: Some(grant("r-2")) };
        let stale = TokenRecord { token: Some("a-1".into()), saved_at: 60, oauth: Some(grant("r-1")) };
        let local = VaultDoc { smartthings: Some(renewed.clone()), ..doc(vec![], vec![]) };
        let remote = VaultDoc { smartthings: Some(stale.clone()), ..doc(vec![], vec![]) };
        assert_eq!(merge(&local, &remote).smartthings, Some(renewed.clone()));
        assert_eq!(merge(&remote, &local).smartthings, Some(renewed.clone()));
        // The grant survives the trip through the vault's JSON, and a version-1 token still reads.
        let json = serde_json::to_string(&local).unwrap();
        assert!(json.contains("\"oauth\":{\"clientId\":\"c\""), "{json}");
        assert_eq!(serde_json::from_str::<VaultDoc>(&json).unwrap(), local);
        let old: VaultDoc = serde_json::from_str(r#"{"format":1,"smartthings":{"token":"t","savedAt":5}}"#).unwrap();
        assert_eq!(old.smartthings, Some(TokenRecord { token: Some("t".into()), saved_at: 5, oauth: None }));
    }

    #[test]
    fn last_seen_alone_isnt_worth_a_write() {
        let a = doc(vec![device("tv", "TV", 1, Some(10))], vec![]);
        let mut b = a.clone();
        b.devices[0].last_seen = Some(99);
        assert!(same_for_vault(&a, &b));
        assert!(!same_for_vault(&a, &doc(vec![device("tv", "TV 2", 1, Some(10))], vec![])));
    }

    #[test]
    fn reads_server_addresses() {
        assert_eq!(normalize_server(" sync.example.com/ ").unwrap(), "https://sync.example.com");
        assert_eq!(normalize_server("http://192.168.3.10:8080").unwrap(), "http://192.168.3.10:8080");
        assert!(normalize_server("").is_err());
        assert!(normalize_server("ftp://x").is_err());
    }

    /// Fixed inputs whose outputs the iPhone app's tests check with CommonCrypto and CryptoKit.
    #[test]
    fn account_key_vectors() {
        let keys = derive_keys("andre@example.com", "correct horse battery");
        println!("auth {}", hex(&keys.auth));
        println!("wrap {}", hex(&keys.wrap));
        let doc = doc(vec![device("tv", "TV", 1, Some(10))], vec![]);
        let sealed = seal(&[5u8; 32], vault_aad("u1").as_bytes(), &serde_json::to_vec(&doc).unwrap()).unwrap();
        println!("vault {}", B64.encode(&sealed));
        assert_eq!(decode(&[5u8; 32], "u1", &json!(B64.encode(&sealed))).unwrap(), doc);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn mac(dir: &Path) -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_app();
        app.manage(Store::open(dir).unwrap());
        app.manage(Account::open(dir));
        app.manage(Vault::open(dir));
        app
    }

    /// Two Macs on one account against a real server: everything each adds, renames and
    /// removes ends up on both.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_macs_converge_through_the_server() {
        let server = Arc::new(thingscan_server::Server::in_memory(true).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, thingscan_server::router(server)).into_future());

        let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (mac(dir_a.path()), mac(dir_b.path()));
        let (a, b) = (a.handle().clone(), b.handle().clone());
        let tv = a.state::<Store>().add(scanned(&["ocf:tv"], "192.168.3.16"), "TV".into(), None).unwrap();
        a.state::<Store>().set_tv_token(&tv.id, "a-token".into()).unwrap();
        a.state::<Account>().save("st-token").unwrap();
        let ac = b.state::<Store>().add(scanned(&["ocf:ac"], "192.168.3.4"), "A/C".into(), None).unwrap();

        let status = a.state::<Vault>().sign_up(&a, &base, "Me@Example.com", "hunter22hunter").await.unwrap();
        assert_eq!((status.signed_in, status.error.as_deref()), (true, None));
        let wrong = b.state::<Vault>().sign_in(&b, &base, "me@example.com", "nope-nope-nope").await;
        assert_eq!(wrong.unwrap_err().0, "Wrong email or password.");
        let status = b.state::<Vault>().sign_in(&b, &base, "me@example.com", "hunter22hunter").await.unwrap();
        assert_eq!(status.error, None);

        // B now has both devices and the token; A picks up B's A/C on its next sync.
        let names = |app: &AppHandle<tauri::test::MockRuntime>| {
            let mut names: Vec<String> = app.state::<Store>().list().into_iter().map(|d| d.name).collect();
            names.sort();
            names
        };
        assert_eq!(names(&b), ["A/C", "TV"]);
        assert_eq!(b.state::<Account>().token().as_deref(), Some("st-token"));
        // The TV pairing didn't come along: B pairs with the TV itself.
        assert_eq!(b.state::<Store>().get(&tv.id).unwrap().tv_token, None);
        b.state::<Store>().set_tv_token(&tv.id, "b-token".into()).unwrap();
        b.state::<Vault>().sync(&b).await;
        a.state::<Vault>().sync(&a).await;
        assert_eq!(names(&a), ["A/C", "TV"]);

        // A renames the TV, B removes the A/C and disconnects SmartThings.
        std::thread::sleep(Duration::from_millis(1100));
        a.state::<Store>().update(&tv.id, "Living room TV".into(), Some("Living room".into())).unwrap();
        b.state::<Store>().remove(&ac.id).unwrap();
        b.state::<Account>().forget().unwrap();
        a.state::<Vault>().sync(&a).await;
        b.state::<Vault>().sync(&b).await;
        a.state::<Vault>().sync(&a).await;
        assert_eq!(names(&a), ["Living room TV"]);
        assert_eq!(names(&b), ["Living room TV"]);
        assert_eq!(b.state::<Store>().get(&tv.id).unwrap().room.as_deref(), Some("Living room"));
        assert_eq!(a.state::<Account>().token(), None);
        assert_eq!(a.state::<Store>().get(&tv.id).unwrap().tv_token.as_deref(), Some("a-token"));
        assert_eq!(b.state::<Store>().get(&tv.id).unwrap().tv_token.as_deref(), Some("b-token"));

        // Signing out keeps the devices and stops syncing.
        let status = b.state::<Vault>().sign_out(&b).await;
        assert!(!status.signed_in);
        assert_eq!(names(&b), ["Living room TV"]);
        assert!(!dir_b.path().join(FILE).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir_a.path().join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
