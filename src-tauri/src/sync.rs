//! Sync with the Thingscan iPhone app over the local network.
//!
//! The Mac advertises `_thingscan._tcp` over Bonjour and serves a small HTTP API. Pairing:
//! the Mac shows a 6-digit code; the phone sends its X25519 public key with
//! `proof = HMAC-SHA256(SHA-256("thingscan-code:" + code), phone_public)`, so the code itself
//! never crosses the network. Both sides derive
//! `key = HKDF-SHA256(salt = "thingscan-sync-v1", ikm = X25519(...), info = phone_public || mac_public)`.
//! Every sync response is sealed with AES-256-GCM under that key (nonce || ciphertext || tag),
//! with the pairing id as associated data, because it carries the SmartThings token and TV
//! pairing tokens.
//!
//! A paired phone can also ask the Mac to wake a TV (`/v1/wake`, sealed the same way): iOS
//! only lets apps with Apple's multicast entitlement broadcast Wake-on-LAN.

use crate::account::{Account, OAuthGrant};
use crate::store::{SavedDevice, Store, StoreError, now};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use x25519_dalek::{PublicKey, StaticSecret};

const FILE: &str = "phones.json";
/// Tried first so the address stays the same between launches; any free port otherwise.
const PREFERRED_PORT: u16 = 47640;
const SERVICE_TYPE: &str = "_thingscan._tcp.local.";
const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);
const CODE_ATTEMPTS: u32 = 5;
const SALT: &[u8] = b"thingscan-sync-v1";
/// How far a wake request's clock may be from the Mac's before it's taken for a replay.
const WAKE_WINDOW: u64 = 120;

/// A phone that paired with this Mac.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phone {
    pub id: String,
    pub name: String,
    pub paired_at: u64,
    #[serde(default)]
    pub last_sync: Option<u64>,
    /// Base64 of the 32-byte AES key. Never leaves this file.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    key: String,
}

struct PendingCode {
    code: String,
    expires: Instant,
    expires_at: u64,
    attempts_left: u32,
}

pub struct Sync {
    path: PathBuf,
    phones: Mutex<Vec<Phone>>,
    code: Mutex<Option<PendingCode>>,
    port: Mutex<Option<u16>>,
    mac_name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PhoneView {
    id: String,
    name: String,
    paired_at: u64,
    last_sync: Option<u64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    mac_name: String,
    port: Option<u16>,
    addresses: Vec<String>,
    phones: Vec<PhoneView>,
    code: Option<CodeView>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CodeView {
    code: String,
    /// Seconds since the epoch.
    expires_at: u64,
}

impl Sync {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let phones = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { path, phones: Mutex::new(phones), code: Mutex::new(None), port: Mutex::new(None), mac_name: mac_name() }
    }

    pub fn status(&self) -> SyncStatus {
        let port = *self.port.lock().unwrap();
        SyncStatus {
            mac_name: self.mac_name.clone(),
            port,
            addresses: port.map(|p| lan_ipv4s().iter().map(|ip| format!("{ip}:{p}")).collect()).unwrap_or_default(),
            phones: self
                .phones
                .lock()
                .unwrap()
                .iter()
                .map(|p| PhoneView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    paired_at: p.paired_at,
                    last_sync: p.last_sync,
                })
                .collect(),
            code: self.active_code().map(|c| CodeView { code: c.code, expires_at: c.expires_at }),
        }
    }

    fn active_code(&self) -> Option<PendingCode> {
        let mut code = self.code.lock().unwrap();
        if code.as_ref().is_some_and(|c| c.expires <= Instant::now() || c.attempts_left == 0) {
            *code = None;
        }
        code.as_ref().map(|c| PendingCode {
            code: c.code.clone(),
            expires: c.expires,
            expires_at: c.expires_at,
            attempts_left: c.attempts_left,
        })
    }

    /// Starts pairing: a fresh 6-digit code, valid for five minutes.
    pub fn start_pairing(&self) -> Result<CodeView, StoreError> {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|e| StoreError::Io(format!("no randomness: {e}")))?;
        let code = format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000);
        let expires_at = now() + CODE_LIFETIME.as_secs();
        *self.code.lock().unwrap() = Some(PendingCode {
            code: code.clone(),
            expires: Instant::now() + CODE_LIFETIME,
            expires_at,
            attempts_left: CODE_ATTEMPTS,
        });
        Ok(CodeView { code, expires_at })
    }

    pub fn cancel_pairing(&self) {
        *self.code.lock().unwrap() = None;
    }

    pub fn remove_phone(&self, id: &str) -> Result<(), StoreError> {
        let mut phones = self.phones.lock().unwrap();
        phones.retain(|p| p.id != id);
        self.write(&phones)
    }

    /// Checks the phone's proof of the code and completes the key exchange.
    fn pair(&self, phone_name: &str, phone_public: [u8; 32], proof: &[u8]) -> Result<(Phone, [u8; 32]), &'static str> {
        let mut pending = self.code.lock().unwrap();
        let Some(code) = pending.as_mut().filter(|c| c.expires > Instant::now() && c.attempts_left > 0) else {
            return Err("no pairing code is showing on the Mac");
        };
        if !verify_proof(&code.code, &phone_public, proof) {
            code.attempts_left -= 1;
            return Err("wrong code");
        }
        *pending = None;
        drop(pending);

        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|_| "no randomness")?;
        let mac_secret = StaticSecret::from(secret);
        let mac_public = PublicKey::from(&mac_secret);
        let shared = mac_secret.diffie_hellman(&PublicKey::from(phone_public));
        let key = derive_key(shared.as_bytes(), &phone_public, mac_public.as_bytes());

        let mut id_bytes = [0u8; 12];
        getrandom::fill(&mut id_bytes).map_err(|_| "no randomness")?;
        let phone = Phone {
            id: hex(&id_bytes),
            name: phone_name.chars().filter(|c| !c.is_control()).take(60).collect::<String>().trim().to_string(),
            paired_at: now(),
            last_sync: None,
            key: B64.encode(key),
        };
        let mut phones = self.phones.lock().unwrap();
        phones.push(phone.clone());
        self.write(&phones).map_err(|_| "couldn't save the pairing")?;
        Ok((phone, mac_public.to_bytes()))
    }

    fn key_for(&self, id: &str) -> Option<[u8; 32]> {
        let phones = self.phones.lock().unwrap();
        let key = B64.decode(&phones.iter().find(|p| p.id == id)?.key).ok()?;
        key.try_into().ok()
    }

    fn touch(&self, id: &str) {
        let mut phones = self.phones.lock().unwrap();
        if let Some(phone) = phones.iter_mut().find(|p| p.id == id) {
            phone.last_sync = Some(now());
        }
        let _ = self.write(&phones);
    }

    /// Keys live here, so the file is readable only by this user.
    fn write(&self, phones: &[Phone]) -> Result<(), StoreError> {
        let json = serde_json::to_vec_pretty(phones).map_err(|e| StoreError::Io(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        write_private(&tmp, &json).map_err(|e| StoreError::Io(format!("can't write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &self.path).map_err(|e| StoreError::Io(format!("can't save {}: {e}", self.path.display())))
    }
}

pub fn code_key(code: &str) -> [u8; 32] {
    Sha256::digest(format!("thingscan-code:{code}").as_bytes()).into()
}

/// What the phone sends to prove it knows the code (the iPhone app computes the same).
#[cfg(test)]
pub fn proof_for(code: &str, phone_public: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(&code_key(code)).expect("HMAC takes any key length");
    mac.update(phone_public);
    mac.finalize().into_bytes().to_vec()
}

fn verify_proof(code: &str, phone_public: &[u8], proof: &[u8]) -> bool {
    let mut mac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(&code_key(code)).expect("HMAC takes any key length");
    mac.update(phone_public);
    mac.verify_slice(proof).is_ok()
}

pub fn derive_key(shared: &[u8], phone_public: &[u8], mac_public: &[u8]) -> [u8; 32] {
    let mut info = phone_public.to_vec();
    info.extend_from_slice(mac_public);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(SALT), shared).expand(&info, &mut key).expect("32 bytes is a valid HKDF length");
    key
}

/// nonce (12) || ciphertext || tag (16): the layout CryptoKit's `AES.GCM.SealedBox(combined:)` reads.
pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, &'static str> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "bad key")?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|_| "no randomness")?;
    let sealed = cipher
        .encrypt(&Nonce::try_from(&nonce[..]).map_err(|_| "bad nonce")?, Payload { msg: plaintext, aad })
        .map_err(|_| "couldn't encrypt")?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let (nonce, body) = sealed.split_at_checked(12)?;
    cipher.decrypt(&Nonce::try_from(nonce).ok()?, Payload { msg: body, aad }).ok()
}

// HTTP

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    phone_name: String,
    public_key: String,
    proof: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncPayload {
    mac_name: String,
    generated_at: u64,
    devices: Vec<SavedDevice>,
    smartthings: Option<SmartThingsToken>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SmartThingsToken {
    token: String,
    saved_at: Option<u64>,
    /// The phone takes newer renewals from the Mac; while it's paired only with the Mac, the
    /// Mac is the one that renews.
    #[serde(skip_serializing_if = "Option::is_none")]
    oauth: Option<OAuthGrant>,
}

#[derive(Deserialize)]
struct SealedRequest {
    sealed: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WakeRequest {
    mac: String,
    sent_at: u64,
}

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(json!({ "error": message })))
}

async fn info<R: Runtime>(AxumState(app): AxumState<AppHandle<R>>) -> Json<serde_json::Value> {
    let sync = app.state::<Sync>();
    Json(json!({
        "app": "thingscan",
        "version": 1,
        "name": sync.mac_name,
        "pairing": sync.active_code().is_some(),
    }))
}

async fn pair<R: Runtime>(
    AxumState(app): AxumState<AppHandle<R>>,
    Json(request): Json<PairRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let phone_public: [u8; 32] = B64
        .decode(&request.public_key)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "bad public key"))?;
    let proof = B64.decode(&request.proof).map_err(|_| error(StatusCode::BAD_REQUEST, "bad proof"))?;
    let sync = app.state::<Sync>();
    let (phone, mac_public) =
        sync.pair(&request.phone_name, phone_public, &proof).map_err(|m| error(StatusCode::FORBIDDEN, m))?;
    let _ = app.emit("sync-changed", json!({ "paired": phone.name }));
    Ok(Json(json!({
        "pairingId": phone.id,
        "macPublicKey": B64.encode(mac_public),
        "macName": sync.mac_name,
    })))
}

/// The pairing a request comes from, and its key.
fn paired<R: Runtime>(
    app: &AppHandle<R>,
    headers: &HeaderMap,
) -> Result<(String, [u8; 32]), (StatusCode, Json<serde_json::Value>)> {
    let id = headers
        .get("x-thingscan-pairing")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "not paired"))?;
    let key = app
        .state::<Sync>()
        .key_for(id)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "this phone isn't paired with the Mac"))?;
    Ok((id.to_string(), key))
}

async fn sync_devices<R: Runtime>(
    AxumState(app): AxumState<AppHandle<R>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (id, key) = paired(&app, &headers)?;
    let id = id.as_str();
    let sync = app.state::<Sync>();
    let account = app.state::<Account>();
    let payload = SyncPayload {
        mac_name: sync.mac_name.clone(),
        generated_at: now(),
        // A TV pairing only works on the device the TV issued it to; the phone pairs on its own.
        devices: app.state::<Store>().list().into_iter().map(|d| SavedDevice { tv_token: None, ..d }).collect(),
        smartthings: account.token().map(|token| SmartThingsToken {
            token,
            saved_at: account.saved_at(),
            oauth: account.record().and_then(|r| r.oauth),
        }),
    };
    let plaintext = serde_json::to_vec(&payload).map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "encoding"))?;
    let sealed = seal(&key, id.as_bytes(), &plaintext).map_err(|m| error(StatusCode::INTERNAL_SERVER_ERROR, m))?;
    sync.touch(id);
    let _ = app.emit("sync-changed", json!({ "synced": id }));
    Ok(Json(json!({ "sealed": B64.encode(sealed) })))
}

/// Sends Wake-on-LAN for the phone, whose own broadcasts iOS blocks.
async fn wake<R: Runtime>(
    AxumState(app): AxumState<AppHandle<R>>,
    headers: HeaderMap,
    Json(body): Json<SealedRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (id, key) = paired(&app, &headers)?;
    let request: WakeRequest = B64
        .decode(&body.sealed)
        .ok()
        .and_then(|sealed| open(&key, id.as_bytes(), &sealed))
        .and_then(|plain| serde_json::from_slice(&plain).ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "the request couldn't be opened"))?;
    if request.sent_at.abs_diff(now()) > WAKE_WINDOW {
        return Err(error(StatusCode::FORBIDDEN, "the request is too old; check the phone's clock"));
    }
    thingscan_core::tv::wake(&request.mac).await.map_err(|e| error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

/// Starts the HTTP server and the Bonjour advertisement. Failures are logged; the rest of
/// the app works without sync.
pub fn start<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, PREFERRED_PORT)).await {
            Ok(listener) => listener,
            Err(_) => match tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
                Ok(listener) => listener,
                Err(e) => {
                    eprintln!("thingscan sync: can't listen: {e}");
                    return;
                }
            },
        };
        let port = listener.local_addr().map(|a: SocketAddr| a.port()).unwrap_or(PREFERRED_PORT);
        let sync = app.state::<Sync>();
        *sync.port.lock().unwrap() = Some(port);
        let advertised = advertise(&sync.mac_name, port);
        if let Err(e) = &advertised {
            eprintln!("thingscan sync: Bonjour advertisement failed: {e}");
        }
        if let Err(e) = axum::serve(listener, router(app.clone())).await {
            eprintln!("thingscan sync: server stopped: {e}");
        }
        drop(advertised);
    });
}

fn router<R: Runtime>(app: AppHandle<R>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/pair", post(pair))
        .route("/v1/sync", get(sync_devices))
        .route("/v1/wake", post(wake))
        .with_state(app)
}

/// Keeps the Bonjour registration alive for as long as the returned daemon lives.
fn advertise(name: &str, port: u16) -> Result<mdns_sd::ServiceDaemon, mdns_sd::Error> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let ips = lan_ipv4s();
    let host =
        format!("{}.local.", name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>());
    let ip_list = ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>().join(",");
    let properties = [("v", "1"), ("name", name), ("port", &port.to_string()), ("ip", &ip_list)];
    let instance = format!("Thingscan on {name}");
    let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, &instance, &host, &ips[..], port, &properties[..])?;
    daemon.register(info)?;
    Ok(daemon)
}

fn lan_ipv4s() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && !i.name.starts_with("utun") && !i.name.starts_with("bridge"))
        .map(|i| i.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(v4) if !v4.is_link_local()))
        .collect()
}

/// The Mac's name as shown in Sharing settings, e.g. "Andre's MacBook Air".
pub(crate) fn mac_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

    fn phone_keys() -> (StaticSecret, [u8; 32]) {
        let secret = StaticSecret::from([7u8; 32]);
        let public = PublicKey::from(&secret).to_bytes();
        (secret, public)
    }

    #[test]
    fn pairs_with_the_right_code_and_both_sides_share_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let sync = Sync::open(dir.path());
        let code = sync.start_pairing().unwrap().code;
        let (phone_secret, phone_public) = phone_keys();

        let (phone, mac_public) = sync.pair("Andre's iPhone", phone_public, &proof_for(&code, &phone_public)).unwrap();
        let shared = phone_secret.diffie_hellman(&PublicKey::from(mac_public));
        let phone_side = derive_key(shared.as_bytes(), &phone_public, &mac_public);
        assert_eq!(sync.key_for(&phone.id), Some(phone_side));

        // The code is single use, and the pairing survives a restart.
        assert!(sync.pair("again", phone_public, &proof_for(&code, &phone_public)).is_err());
        assert_eq!(Sync::open(dir.path()).key_for(&phone.id), Some(phone_side));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn wrong_codes_run_out() {
        let dir = tempfile::tempdir().unwrap();
        let sync = Sync::open(dir.path());
        let code = sync.start_pairing().unwrap().code;
        let wrong = if code == "000000" { "111111" } else { "000000" };
        let (_, phone_public) = phone_keys();
        for _ in 0..CODE_ATTEMPTS {
            assert_eq!(sync.pair("x", phone_public, &proof_for(wrong, &phone_public)).unwrap_err(), "wrong code");
        }
        // Even the right code no longer works once the attempts are used up.
        assert!(sync.pair("x", phone_public, &proof_for(&code, &phone_public)).is_err());
        assert!(sync.status().code.is_none());
    }

    #[test]
    fn sealed_payloads_open_only_with_the_key_and_pairing_id() {
        let key = [3u8; 32];
        let sealed = seal(&key, b"pairing-1", b"{\"devices\":[]}").unwrap();
        assert_eq!(open(&key, b"pairing-1", &sealed).as_deref(), Some(&b"{\"devices\":[]}"[..]));
        assert!(open(&key, b"pairing-2", &sealed).is_none());
        assert!(open(&[4u8; 32], b"pairing-1", &sealed).is_none());
        assert_eq!(sealed.len(), 12 + 14 + 16);
    }

    /// The routes the iPhone app calls, with the payload it decrypts.
    #[tokio::test]
    async fn pairs_and_syncs_over_http() {
        use crate::store::tests::scanned;
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_app();
        let store = Store::open(dir.path()).unwrap();
        let ac = store
            .add(scanned(&["ocf:d1"], "192.168.3.7"), "Living room A/C".into(), Some("Living room".into()))
            .unwrap();
        store.set_tv_token(&ac.id, "mac-only".into()).unwrap();
        app.manage(store);
        app.manage(Account::open(dir.path()));
        app.manage(Sync::open(dir.path()));
        app.state::<Account>().save("st-token-123").unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, router(app.handle().clone())).into_future());
        let client = reqwest::Client::new();
        let (phone_secret, phone_public) = phone_keys();
        let pair = |code: &str| {
            client.post(format!("{base}/v1/pair")).json(&json!({
                "phoneName": "Andre's iPhone",
                "publicKey": B64.encode(phone_public),
                "proof": B64.encode(proof_for(code, &phone_public)),
            }))
        };

        let refused = pair("000000").send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            refused.json::<serde_json::Value>().await.unwrap()["error"],
            "no pairing code is showing on the Mac"
        );

        let code = app.state::<Sync>().start_pairing().unwrap().code;
        let info: serde_json::Value = client.get(format!("{base}/v1/info")).send().await.unwrap().json().await.unwrap();
        assert_eq!(info["pairing"], true);
        let reply: serde_json::Value = pair(&code).send().await.unwrap().json().await.unwrap();
        let id = reply["pairingId"].as_str().unwrap();
        let mac_public: [u8; 32] = B64.decode(reply["macPublicKey"].as_str().unwrap()).unwrap().try_into().unwrap();
        let shared = phone_secret.diffie_hellman(&PublicKey::from(mac_public));
        let key = derive_key(shared.as_bytes(), &phone_public, &mac_public);

        let unknown = client.get(format!("{base}/v1/sync")).header("x-thingscan-pairing", "nope").send().await.unwrap();
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        let synced: serde_json::Value = client
            .get(format!("{base}/v1/sync"))
            .header("x-thingscan-pairing", id)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let sealed = B64.decode(synced["sealed"].as_str().unwrap()).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&open(&key, id.as_bytes(), &sealed).unwrap()).unwrap();
        assert_eq!(payload["devices"][0]["id"], ac.id.as_str());
        assert_eq!(payload["devices"][0]["room"], "Living room");
        assert!(
            payload["devices"][0]["tvToken"].is_null(),
            "the TV only honours a pairing on the device it gave it to"
        );
        assert_eq!(payload["smartthings"]["token"], "st-token-123");
        assert!(payload["smartthings"].get("oauth").is_none(), "a personal token has no grant");
        assert!(app.state::<Sync>().status().phones[0].last_sync.is_some());

        // Wake requests: only from a paired phone, sealed with its key, and fresh.
        let wake = |pairing: &str, body: serde_json::Value| {
            client.post(format!("{base}/v1/wake")).header("x-thingscan-pairing", pairing).json(&body).send()
        };
        let sealed_wake = |mac: &str, sent_at: u64| {
            let plain = serde_json::to_vec(&json!({ "mac": mac, "sentAt": sent_at })).unwrap();
            json!({ "sealed": B64.encode(seal(&key, id.as_bytes(), &plain).unwrap()) })
        };
        let stranger = wake("nope", sealed_wake("04:E4:B6:B3:90:82", now())).await.unwrap();
        assert_eq!(stranger.status(), StatusCode::UNAUTHORIZED);
        let forged = wake(id, json!({ "sealed": B64.encode([0u8; 40]) })).await.unwrap();
        assert_eq!(forged.status(), StatusCode::BAD_REQUEST);
        let replayed = wake(id, sealed_wake("04:E4:B6:B3:90:82", now() - 600)).await.unwrap();
        assert_eq!(replayed.status(), StatusCode::FORBIDDEN);
        // A MAC that doesn't parse fails before anything goes on the network.
        let bad = wake(id, sealed_wake("04:E4:B6", now())).await.unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert!(bad.json::<serde_json::Value>().await.unwrap()["error"].as_str().unwrap().contains("MAC"));
    }

    /// Fixed inputs whose outputs the iPhone app's tests check with CryptoKit.
    #[test]
    fn cross_platform_vectors() {
        let phone_public = PublicKey::from(&StaticSecret::from([1u8; 32])).to_bytes();
        let mac_secret = StaticSecret::from([2u8; 32]);
        let mac_public = PublicKey::from(&mac_secret).to_bytes();
        let shared = mac_secret.diffie_hellman(&PublicKey::from(phone_public));
        println!("phone_public {}", hex(&phone_public));
        println!("mac_public   {}", hex(&mac_public));
        println!("key          {}", hex(&derive_key(shared.as_bytes(), &phone_public, &mac_public)));
        println!("proof        {}", hex(&proof_for("123456", &phone_public)));
    }
}
