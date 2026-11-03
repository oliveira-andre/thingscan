//! Thingscan's account server: email + password accounts, each with one end-to-end encrypted
//! vault that the Mac and iPhone apps sync through.
//!
//! The server never sees a password or what's in a vault. Clients stretch the password with
//! PBKDF2 and send only a derived `authKey`, which is stored as an Argon2id hash. The vault
//! (devices, TV pairings, the SmartThings token) arrives as AES-256-GCM ciphertext under a key
//! that only the clients hold, wrapped with another password-derived key (`wrappedKey`).
//!
//! The vault is versioned: a write names the version it was based on, and is refused with the
//! current vault when someone else wrote first, so the client can merge and try again.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A session unused for this long has to sign in again.
const SESSION_IDLE: u64 = 180 * 24 * 3600;
/// Wrong passwords allowed per email within `FAILURE_WINDOW`.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const MAX_BODY: usize = 2 * 1024 * 1024;
/// nonce (12) + a 32-byte key + tag (16).
const WRAPPED_KEY_LEN: usize = 60;

pub struct Server {
    db: Mutex<Connection>,
    allow_signup: bool,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// Checked against when the email is unknown, so a miss takes as long as a wrong password.
    dummy_hash: String,
}

#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self(status, message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("thingscan-server: database: {e}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "The server couldn't save that. Try again.")
    }
}

type ApiResult<T> = Result<T, ApiError>;

impl Server {
    pub fn open(path: &Path, allow_signup: bool) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?, allow_signup)
    }

    pub fn in_memory(allow_signup: bool) -> rusqlite::Result<Self> {
        Self::with(Connection::open_in_memory()?, allow_signup)
    }

    fn with(db: Connection, allow_signup: bool) -> rusqlite::Result<Self> {
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS users (
                 id TEXT PRIMARY KEY,
                 email TEXT NOT NULL UNIQUE,
                 auth_hash TEXT NOT NULL,
                 wrapped_key TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 token_hash TEXT PRIMARY KEY,
                 user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 device TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 last_used_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS vaults (
                 user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                 version INTEGER NOT NULL,
                 data TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )?;
        db.execute("DELETE FROM sessions WHERE last_used_at < ?1", params![now().saturating_sub(SESSION_IDLE)])?;
        let dummy_hash = hash_auth(&random::<32>()).expect("argon2 accepts a 32-byte input");
        Ok(Self { db: Mutex::new(db), allow_signup, failures: Mutex::new(HashMap::new()), dummy_hash })
    }

    /// Removes an account, its sessions and its vault. For an owner who forgot their password:
    /// the vault can't be opened without it, so starting over is the only way back.
    pub fn delete_user(&self, email: &str) -> rusqlite::Result<bool> {
        let email = email.trim().to_lowercase();
        Ok(self.db.lock().unwrap().execute("DELETE FROM users WHERE email = ?1", params![email])? > 0)
    }

    /// Every account: email, when it was created, and when its vault last changed.
    pub fn list_users(&self) -> rusqlite::Result<Vec<(String, u64, Option<u64>)>> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(
            "SELECT users.email, users.created_at, vaults.updated_at FROM users
             LEFT JOIN vaults ON vaults.user_id = users.id ORDER BY users.created_at",
        )?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect()
    }

    fn new_session(&self, user_id: &str, device: &str) -> ApiResult<String> {
        let token = B64URL.encode(random::<32>());
        let device: String = device.chars().filter(|c| !c.is_control()).take(60).collect();
        self.db.lock().unwrap().execute(
            "INSERT INTO sessions (token_hash, user_id, device, created_at, last_used_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![token_hash(&token), user_id, device.trim(), now()],
        )?;
        Ok(token)
    }

    /// The user behind the request's `Authorization: Bearer <token>`.
    fn authenticate(&self, headers: &HeaderMap) -> ApiResult<String> {
        let unauthorized = || ApiError::new(StatusCode::UNAUTHORIZED, "Signed out. Sign in again.");
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let db = self.db.lock().unwrap();
        let hash = token_hash(token);
        let user: Option<(String, u64)> = db
            .query_row("SELECT user_id, last_used_at FROM sessions WHERE token_hash = ?1", params![hash], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        match user {
            Some((user_id, last_used)) if last_used + SESSION_IDLE > now() => {
                db.execute("UPDATE sessions SET last_used_at = ?1 WHERE token_hash = ?2", params![now(), hash])?;
                Ok(user_id)
            }
            Some(_) => {
                db.execute("DELETE FROM sessions WHERE token_hash = ?1", params![hash])?;
                Err(unauthorized())
            }
            None => Err(unauthorized()),
        }
    }

    fn too_many_failures(&self, email: &str) -> bool {
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|_, (_, since)| since.elapsed() < FAILURE_WINDOW);
        failures.get(email).is_some_and(|(count, _)| *count >= MAX_FAILURES)
    }

    fn note_failure(&self, email: &str) {
        let mut failures = self.failures.lock().unwrap();
        failures.entry(email.to_string()).or_insert((0, Instant::now())).0 += 1;
    }
}

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/signup", post(signup))
        .route("/v1/login", post(login))
        .route("/v1/logout", post(logout))
        .route("/v1/vault", get(get_vault).put(put_vault))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(server)
}

async fn health() -> Json<Value> {
    Json(json!({ "app": "thingscan-server", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignupRequest {
    email: String,
    auth_key: String,
    wrapped_key: String,
    #[serde(default)]
    device: String,
}

async fn signup(
    State(server): State<Arc<Server>>,
    Json(request): Json<SignupRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !server.allow_signup {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "This server doesn't take new accounts."));
    }
    let email = normalize_email(&request.email)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "Enter a valid email address."))?;
    let auth_key = decode_auth_key(&request.auth_key)?;
    if B64.decode(&request.wrapped_key).map(|k| k.len()) != Ok(WRAPPED_KEY_LEN) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad wrapped key"));
    }
    let auth_hash = blocking(move || hash_auth(&auth_key)).await?;
    let user_id = hex(&random::<12>());
    let inserted = server.db.lock().unwrap().execute(
        "INSERT INTO users (id, email, auth_hash, wrapped_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![user_id, email, auth_hash, request.wrapped_key, now()],
    );
    match inserted {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "There's already an account with this email. Sign in instead.",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let token = server.new_session(&user_id, &request.device)?;
    Ok((StatusCode::CREATED, Json(json!({ "userId": user_id, "email": email, "token": token }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest {
    email: String,
    auth_key: String,
    #[serde(default)]
    device: String,
}

async fn login(State(server): State<Arc<Server>>, Json(request): Json<LoginRequest>) -> ApiResult<Json<Value>> {
    let wrong = || ApiError::new(StatusCode::UNAUTHORIZED, "Wrong email or password.");
    let email = normalize_email(&request.email).ok_or_else(wrong)?;
    if server.too_many_failures(&email) {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "Too many wrong passwords. Try again in 15 minutes."));
    }
    let auth_key = decode_auth_key(&request.auth_key)?;
    let user: Option<(String, String, String)> = server
        .db
        .lock()
        .unwrap()
        .query_row("SELECT id, auth_hash, wrapped_key FROM users WHERE email = ?1", params![email], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let hash = user.as_ref().map_or_else(|| server.dummy_hash.clone(), |(_, hash, _)| hash.clone());
    let matches = blocking(move || Ok(verify_auth(&auth_key, &hash))).await?;
    let Some((user_id, _, wrapped_key)) = user.filter(|_| matches) else {
        server.note_failure(&email);
        return Err(wrong());
    };
    server.failures.lock().unwrap().remove(&email);
    let token = server.new_session(&user_id, &request.device)?;
    Ok(Json(json!({ "userId": user_id, "email": email, "token": token, "wrappedKey": wrapped_key })))
}

async fn logout(State(server): State<Arc<Server>>, headers: HeaderMap) -> ApiResult<StatusCode> {
    server.authenticate(&headers)?;
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    if let Some(token) = token {
        server.db.lock().unwrap().execute("DELETE FROM sessions WHERE token_hash = ?1", params![token_hash(token)])?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct VaultQuery {
    /// The version the client already has; the reply leaves the data out when it's still current.
    known: Option<u64>,
}

async fn get_vault(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<VaultQuery>,
) -> ApiResult<Json<Value>> {
    let user_id = server.authenticate(&headers)?;
    let vault: Option<(u64, String, u64)> = server
        .db
        .lock()
        .unwrap()
        .query_row("SELECT version, data, updated_at FROM vaults WHERE user_id = ?1", params![user_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    Ok(Json(match vault {
        None => json!({ "version": 0, "vault": null, "updatedAt": null }),
        Some((version, _, updated_at)) if query.known == Some(version) => {
            json!({ "version": version, "unchanged": true, "updatedAt": updated_at })
        }
        Some((version, data, updated_at)) => json!({ "version": version, "vault": data, "updatedAt": updated_at }),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutVault {
    base_version: u64,
    vault: String,
}

async fn put_vault(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Json(request): Json<PutVault>,
) -> ApiResult<Response> {
    let user_id = server.authenticate(&headers)?;
    if B64.decode(&request.vault).is_err() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad vault"));
    }
    let mut db = server.db.lock().unwrap();
    let tx = db.transaction()?;
    let current: Option<(u64, String)> = tx
        .query_row("SELECT version, data FROM vaults WHERE user_id = ?1", params![user_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let current_version = current.as_ref().map_or(0, |(v, _)| *v);
    if request.base_version != current_version {
        // Someone else wrote first: hand back what's there to merge with.
        let (version, data) = current.unwrap_or((0, String::new()));
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "The vault changed; merge and try again.", "version": version, "vault": data })),
        )
            .into_response());
    }
    let version = current_version + 1;
    tx.execute(
        "INSERT INTO vaults (user_id, version, data, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(user_id) DO UPDATE SET version = ?2, data = ?3, updated_at = ?4",
        params![user_id, version, request.vault, now()],
    )?;
    tx.commit()?;
    Ok(Json(json!({ "version": version })).into_response())
}

// Helpers

const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the OS has randomness");
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Trimmed and lowercased, or None when it doesn't look like an address.
pub fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_lowercase();
    let (local, domain) = email.split_once('@')?;
    let valid = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && email.len() <= 254
        && !email.chars().any(char::is_whitespace);
    valid.then_some(email)
}

fn decode_auth_key(text: &str) -> ApiResult<Vec<u8>> {
    B64.decode(text)
        .ok()
        .filter(|k| k.len() == 32)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad auth key"))
}

fn hash_auth(auth_key: &[u8]) -> ApiResult<String> {
    let salt = SaltString::encode_b64(&random::<16>()).expect("16 bytes is a valid salt");
    Argon2::default()
        .hash_password(auth_key, &salt)
        .map(|h| h.to_string())
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "couldn't hash"))
}

fn verify_auth(auth_key: &[u8], hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| Argon2::default().verify_password(auth_key, &parsed).is_ok())
}

/// Argon2 takes tens of milliseconds of CPU; keep it off the async workers.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> ApiResult<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "worker stopped"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn start(allow_signup: bool) -> String {
        let server = Arc::new(Server::in_memory(allow_signup).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, router(server)).into_future());
        base
    }

    fn key(byte: u8) -> String {
        B64.encode([byte; 32])
    }

    fn wrapped() -> String {
        B64.encode([9u8; WRAPPED_KEY_LEN])
    }

    async fn post(base: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let response = reqwest::Client::new().post(format!("{base}{path}")).json(&body).send().await.unwrap();
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn vault(base: &str, token: &str, method: reqwest::Method, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = reqwest::Client::new().request(method, format!("{base}/v1/vault")).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn signs_up_signs_in_and_keeps_one_versioned_vault() {
        let base = start(true).await;
        let (status, created) = post(
            &base,
            "/v1/signup",
            json!({ "email": " Andre@Example.com ", "authKey": key(1), "wrappedKey": wrapped(), "device": "Mac" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["email"], "andre@example.com");
        let mac = created["token"].as_str().unwrap().to_string();

        let (status, _) = post(
            &base,
            "/v1/signup",
            json!({ "email": "andre@example.com", "authKey": key(2), "wrappedKey": wrapped() }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, _) = post(&base, "/v1/login", json!({ "email": "andre@example.com", "authKey": key(2) })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = post(&base, "/v1/login", json!({ "email": "nobody@example.com", "authKey": key(1) })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, signed_in) =
            post(&base, "/v1/login", json!({ "email": "ANDRE@example.com", "authKey": key(1), "device": "iPhone" }))
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(signed_in["wrappedKey"], wrapped());
        assert_eq!(signed_in["userId"], created["userId"]);
        let phone = signed_in["token"].as_str().unwrap().to_string();

        // Empty to start with; the first write is based on version 0.
        let (_, empty) = vault(&base, &mac, reqwest::Method::GET, None).await;
        assert_eq!(empty["version"], 0);
        assert!(empty["vault"].is_null());
        let (status, first) =
            vault(&base, &mac, reqwest::Method::PUT, Some(json!({ "baseVersion": 0, "vault": "AAAA" }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["version"], 1);

        // The phone writes based on a stale version and gets the current vault back.
        let (status, conflict) =
            vault(&base, &phone, reqwest::Method::PUT, Some(json!({ "baseVersion": 0, "vault": "BBBB" }))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!((conflict["version"].as_u64(), conflict["vault"].as_str()), (Some(1), Some("AAAA")));
        let (status, _) =
            vault(&base, &phone, reqwest::Method::PUT, Some(json!({ "baseVersion": 1, "vault": "CCCC" }))).await;
        assert_eq!(status, StatusCode::OK);

        let response = reqwest::Client::new()
            .get(format!("{base}/v1/vault?known=2"))
            .bearer_auth(&mac)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(response["unchanged"], true);
        let (_, latest) = vault(&base, &mac, reqwest::Method::GET, None).await;
        assert_eq!((latest["version"].as_u64(), latest["vault"].as_str()), (Some(2), Some("CCCC")));

        // Signing out ends that session only.
        let status =
            reqwest::Client::new().post(format!("{base}/v1/logout")).bearer_auth(&phone).send().await.unwrap().status();
        assert_eq!(status.as_u16(), 204);
        assert_eq!(vault(&base, &phone, reqwest::Method::GET, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(vault(&base, &mac, reqwest::Method::GET, None).await.0, StatusCode::OK);
        assert_eq!(vault(&base, "made-up", reqwest::Method::GET, None).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn locks_an_email_after_too_many_wrong_passwords() {
        let base = start(true).await;
        post(&base, "/v1/signup", json!({ "email": "a@b.co", "authKey": key(1), "wrappedKey": wrapped() })).await;
        for _ in 0..MAX_FAILURES {
            assert_eq!(
                post(&base, "/v1/login", json!({ "email": "a@b.co", "authKey": key(7) })).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        let (status, _) = post(&base, "/v1/login", json!({ "email": "a@b.co", "authKey": key(1) })).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn refuses_bad_input_and_closed_signups() {
        let base = start(true).await;
        let bad = [
            json!({ "email": "not-an-email", "authKey": key(1), "wrappedKey": wrapped() }),
            json!({ "email": "a@b.co", "authKey": B64.encode([1u8; 8]), "wrappedKey": wrapped() }),
            json!({ "email": "a@b.co", "authKey": key(1), "wrappedKey": B64.encode([1u8; 10]) }),
        ];
        for body in bad {
            assert_eq!(post(&base, "/v1/signup", body).await.0, StatusCode::BAD_REQUEST);
        }
        let closed = start(false).await;
        let (status, body) =
            post(&closed, "/v1/signup", json!({ "email": "a@b.co", "authKey": key(1), "wrappedKey": wrapped() })).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "This server doesn't take new accounts.");
    }

    #[test]
    fn deletes_and_lists_users() {
        let server = Server::in_memory(true).unwrap();
        server
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO users (id, email, auth_hash, wrapped_key, created_at) VALUES ('u1', 'a@b.co', 'x', 'y', 1)",
                [],
            )
            .unwrap();
        assert_eq!(server.list_users().unwrap(), vec![("a@b.co".to_string(), 1, None)]);
        assert!(server.delete_user(" A@B.co ").unwrap());
        assert!(!server.delete_user("a@b.co").unwrap());
        assert!(server.list_users().unwrap().is_empty());
    }

    #[test]
    fn normalizes_emails() {
        assert_eq!(normalize_email(" Me@Home.Net ").as_deref(), Some("me@home.net"));
        for bad in ["", "me", "me@", "@home.net", "me@home", "me@.net", "me@home.", "m e@home.net", "a@b@c.d"] {
            assert_eq!(normalize_email(bad), None, "{bad}");
        }
    }
}
