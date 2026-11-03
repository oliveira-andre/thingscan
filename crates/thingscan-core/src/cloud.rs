//! The SmartThings cloud API (api.smartthings.com). The SmartThings app controls appliances
//! through it: their local OCF control resources (`/power/vs/0`, `/mode/vs/0`, …) answer
//! plain CoAP with 4.03 and only accept the DTLS credentials of the account that owns them.
//!
//! Personal access tokens made since 30 December 2024 last 24 hours. What lasts is OAuth through
//! an "OAuth-In" app of the user's own (`API_ONLY`, `CONNECTED_SERVICE`, as `smartthings
//! apps:create` makes): its access token also lasts 24 hours, but the refresh token that comes
//! with it buys a new pair. A refresh token works once, expires after 29 days unused, and using
//! it retires the previous access token too.

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;

pub const API: &str = "https://api.smartthings.com/v1";
pub const AUTHORIZE_URL: &str = "https://api.smartthings.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.smartthings.com/oauth/token";
/// Where SmartThings sends the browser back with the code. Thingscan's sign-in window stops
/// there before the page loads; it's the redirect SmartThings' own examples use.
pub const REDIRECT_URI: &str = "https://httpbin.org/get";
/// What Thingscan's app asks for: read, rename and control devices, and read locations.
pub const SCOPES: [&str; 4] = ["r:devices:*", "w:devices:*", "x:devices:*", "r:locations:*"];

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    token: String,
    base: String,
}

/// A device in the user's SmartThings account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDevice {
    pub device_id: String,
    /// The user's label, or the device's own name.
    pub name: String,
    pub manufacturer: Option<String>,
    /// SmartThings category of the main component, e.g. `AirConditioner`, `Washer`, `Television`.
    pub category: Option<String>,
    /// OCF device type, e.g. `oic.d.airconditioner`.
    pub ocf_type: Option<String>,
    pub model: Option<String>,
    /// Capabilities of the main component.
    pub capabilities: Vec<String>,
    /// The OCF device id, which is what a local scan sees as `ocf:<di>`.
    #[serde(default)]
    pub di: Option<String>,
}

/// The user's OAuth-In app. The secret is shown once, when the app is made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
}

/// A fresh pair from the token endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds the access token lasts.
    pub expires_in: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CloudError {
    Unauthorized,
    Forbidden(String),
    Api {
        status: u16,
        message: String,
    },
    Network(String),
    /// The token endpoint refused the code or refresh token: used already, expired or revoked.
    Grant(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloudError::Unauthorized => f.write_str(
                "SmartThings didn't accept the token: it's wrong or expired (tokens made since 30 Dec 2024 last 24 hours)",
            ),
            CloudError::Forbidden(m) => write!(f, "the token isn't allowed to do that ({m}); it needs the Devices scopes"),
            CloudError::Api { status, message } => write!(f, "SmartThings said: {message} ({status})"),
            CloudError::Network(m) => write!(f, "couldn't reach SmartThings: {m}"),
            CloudError::Grant(m) => write!(f, "SmartThings refused Thingscan's sign-in: {m}"),
        }
    }
}

impl std::error::Error for CloudError {}

impl Client {
    pub fn new(token: &str) -> Self {
        Self::with_base(token, API)
    }

    pub fn with_base(token: &str, base: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
        Self { http, token: token.trim().to_string(), base: base.trim_end_matches('/').to_string() }
    }

    async fn request(&self, method: reqwest::Method, url: &str, body: Option<Value>) -> Result<Value, CloudError> {
        let mut request = self.http.request(method, url).bearer_auth(&self.token).header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| CloudError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| CloudError::Network(e.to_string()))?;
        let value: Value =
            if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        match status {
            200..=299 => Ok(value),
            401 => Err(CloudError::Unauthorized),
            403 => Err(CloudError::Forbidden(error_message(&value, status))),
            _ => Err(CloudError::Api { status, message: error_message(&value, status) }),
        }
    }

    async fn get(&self, url: &str) -> Result<Value, CloudError> {
        self.request(reqwest::Method::GET, url, None).await
    }

    /// Every device in the account, following pagination.
    pub async fn devices(&self) -> Result<Vec<CloudDevice>, CloudError> {
        let mut out = Vec::new();
        let mut url = format!("{}/devices", self.base);
        for _ in 0..50 {
            let page = self.get(&url).await?;
            let (items, next) = parse_devices(&page);
            out.extend(items);
            match next {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// Fills in `di` for OCF devices, so they can be matched with what a local scan found.
    pub async fn fill_ocf_ids(&self, devices: &mut [CloudDevice]) {
        let wanted: Vec<(usize, String)> = devices
            .iter()
            .enumerate()
            .filter(|(_, d)| d.capabilities.iter().any(|c| c == "ocf"))
            .map(|(i, d)| (i, d.device_id.clone()))
            .collect();
        let found: Vec<(usize, Option<String>)> = stream::iter(wanted)
            .map(|(i, id)| async move {
                let url = format!("{}/devices/{id}/components/main/capabilities/ocf/status", self.base);
                (i, self.get(&url).await.ok().and_then(|v| v["di"]["value"].as_str().map(str::to_string)))
            })
            .buffer_unordered(6)
            .collect()
            .await;
        for (i, di) in found {
            devices[i].di = di;
        }
    }

    /// `{components: {main: {<capability>: {<attribute>: {value, unit}}}}}`
    pub async fn status(&self, device_id: &str) -> Result<Value, CloudError> {
        self.get(&format!("{}/devices/{}/status", self.base, encode(device_id))).await
    }

    /// `ONLINE`, `OFFLINE` or `UNHEALTHY`.
    pub async fn health(&self, device_id: &str) -> Result<String, CloudError> {
        let value = self.get(&format!("{}/devices/{}/health", self.base, encode(device_id))).await?;
        Ok(value["state"].as_str().unwrap_or("UNKNOWN").to_string())
    }

    /// Makes Thingscan's OAuth-In app in the account this (personal) token belongs to. The token
    /// needs the Apps scope as well as Devices.
    pub async fn create_oauth_app(&self) -> Result<OAuthApp, CloudError> {
        let name = format!("thingscan-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let body = oauth_app_request(&name);
        let reply = self.request(reqwest::Method::POST, &format!("{}/apps", self.base), Some(body)).await?;
        match (reply["oauthClientId"].as_str(), reply["oauthClientSecret"].as_str()) {
            (Some(id), Some(secret)) => Ok(OAuthApp { client_id: id.into(), client_secret: secret.into() }),
            _ => Err(CloudError::Api { status: 200, message: "the new app came without OAuth details".into() }),
        }
    }

    pub async fn command(
        &self,
        device_id: &str,
        component: &str,
        capability: &str,
        command: &str,
        arguments: Vec<Value>,
    ) -> Result<(), CloudError> {
        let url = format!("{}/devices/{}/commands", self.base, encode(device_id));
        let body = json!({ "commands": [command_body(component, capability, command, arguments)] });
        self.request(reqwest::Method::POST, &url, Some(body)).await.map(|_| ())
    }
}

/// The body for `POST /apps`, the same one `smartthings apps:create` sends for an OAuth-In app.
pub fn oauth_app_request(name: &str) -> Value {
    json!({
        "appName": name,
        "displayName": "Thingscan",
        "description": "Lets Thingscan on your Mac and iPhone control your appliances.",
        "appType": "API_ONLY",
        "classifications": ["CONNECTED_SERVICE"],
        "singleInstance": true,
        "principalType": "LOCATION",
        "apiOnly": {},
        "oauth": { "clientName": "Thingscan", "scope": SCOPES, "redirectUris": [REDIRECT_URI] },
    })
}

/// The page where the user signs in to SmartThings and allows the app. `scoped` asks for
/// [`SCOPES`]; without it, SmartThings offers whatever the app was made with.
pub fn authorize_url(client_id: &str, state: &str, scoped: bool) -> String {
    let mut params = vec![
        ("client_id", client_id.to_string()),
        ("response_type", "code".into()),
        ("redirect_uri", REDIRECT_URI.into()),
        ("state", state.into()),
    ];
    if scoped {
        params.push(("scope", SCOPES.join(" ")));
    }
    reqwest::Url::parse_with_params(AUTHORIZE_URL, &params).map(String::from).unwrap_or_default()
}

/// Trades the code from the sign-in page for the first pair.
pub async fn exchange_code(app: &OAuthApp, code: &str) -> Result<Grant, CloudError> {
    token_request(
        TOKEN_URL,
        app,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &app.client_id),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
        ],
    )
    .await
}

/// Trades the refresh token for a new pair. Both old tokens stop working.
pub async fn refresh(app: &OAuthApp, refresh_token: &str) -> Result<Grant, CloudError> {
    refresh_at(TOKEN_URL, app, refresh_token).await
}

pub async fn refresh_at(url: &str, app: &OAuthApp, refresh_token: &str) -> Result<Grant, CloudError> {
    token_request(
        url,
        app,
        &[("grant_type", "refresh_token"), ("client_id", &app.client_id), ("refresh_token", refresh_token)],
    )
    .await
}

async fn token_request(url: &str, app: &OAuthApp, form: &[(&str, &str)]) -> Result<Grant, CloudError> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
    let response = http
        .post(url)
        .basic_auth(&app.client_id, Some(&app.client_secret))
        .header("Accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| CloudError::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| CloudError::Network(e.to_string()))?;
    parse_grant(status, &serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Reads the token endpoint's reply.
pub fn parse_grant(status: u16, value: &Value) -> Result<Grant, CloudError> {
    if (200..300).contains(&status) {
        let s = |key: &str| value[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        return match (s("access_token"), s("refresh_token")) {
            (Some(access_token), Some(refresh_token)) => {
                Ok(Grant { access_token, refresh_token, expires_in: value["expires_in"].as_u64().unwrap_or(86_399) })
            }
            _ => Err(CloudError::Api { status, message: "the reply had no tokens".into() }),
        };
    }
    let reason = value["error_description"].as_str().or_else(|| value["error"].as_str());
    match (value["error"].as_str(), status) {
        (Some("invalid_grant"), _) => Err(CloudError::Grant(reason.unwrap_or("invalid_grant").to_string())),
        (Some("invalid_client" | "unauthorized_client"), _) | (_, 401) => {
            Err(CloudError::Grant(reason.map_or("the app's client ID or secret is wrong".into(), str::to_string)))
        }
        _ => Err(CloudError::Api { status, message: reason.map_or_else(|| format!("HTTP {status}"), str::to_string) }),
    }
}

/// Device ids are UUIDs; anything else must not reach the URL path unescaped.
fn encode(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

pub fn command_body(component: &str, capability: &str, command: &str, arguments: Vec<Value>) -> Value {
    let mut body = json!({ "component": component, "capability": capability, "command": command });
    if !arguments.is_empty() {
        body["arguments"] = Value::Array(arguments);
    }
    body
}

/// One page of `GET /devices`: the devices and the next page's URL.
pub fn parse_devices(page: &Value) -> (Vec<CloudDevice>, Option<String>) {
    let items = page["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    let devices = items
        .iter()
        .filter_map(|item| {
            let device_id = item["deviceId"].as_str()?.to_string();
            let main = item["components"].as_array()?.iter().find(|c| c["id"] == "main");
            let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
            Some(CloudDevice {
                name: s(&item["label"]).or_else(|| s(&item["name"])).unwrap_or_else(|| device_id.clone()),
                device_id,
                manufacturer: s(&item["manufacturerName"]).or_else(|| s(&item["ocf"]["manufacturerName"])),
                category: main.and_then(|m| m["categories"].as_array()?.first().and_then(|c| s(&c["name"]))),
                ocf_type: s(&item["ocf"]["ocfDeviceType"]),
                model: s(&item["ocf"]["modelNumber"]).map(|m| m.split('|').next().unwrap_or_default().to_string()),
                capabilities: main
                    .and_then(|m| m["capabilities"].as_array())
                    .map(|caps| caps.iter().filter_map(|c| s(&c["id"])).collect())
                    .unwrap_or_default(),
                di: None,
            })
        })
        .collect();
    let next = page["_links"]["next"]["href"].as_str().map(str::to_string);
    (devices, next)
}

fn error_message(value: &Value, status: u16) -> String {
    value["error"]["message"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("HTTP {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_device_page() {
        // Shape of a real Samsung room A/C entry (pysmartthings fixture devices_18.json).
        let page = json!({
            "items": [{
                "deviceId": "d424e86b-15cf-79e2-48d5-480a6c0d18f9",
                "name": "[room a/c] Samsung",
                "label": "Bedroom A/C",
                "manufacturerName": "Samsung Electronics",
                "components": [{
                    "id": "main",
                    "capabilities": [{ "id": "ocf", "version": 1 }, { "id": "switch", "version": 1 }],
                    "categories": [{ "name": "AirConditioner", "categoryType": "manufacturer" }]
                }],
                "ocf": { "ocfDeviceType": "oic.d.airconditioner", "modelNumber": "ARA-WW-TP2-21-OT9700|10236341|6001" }
            }],
            "_links": { "next": { "href": "https://api.smartthings.com/v1/devices?page=1" } }
        });
        let (devices, next) = parse_devices(&page);
        assert_eq!(next.as_deref(), Some("https://api.smartthings.com/v1/devices?page=1"));
        let ac = &devices[0];
        assert_eq!(ac.name, "Bedroom A/C");
        assert_eq!(ac.category.as_deref(), Some("AirConditioner"));
        assert_eq!(ac.ocf_type.as_deref(), Some("oic.d.airconditioner"));
        assert_eq!(ac.model.as_deref(), Some("ARA-WW-TP2-21-OT9700"));
        assert_eq!(ac.capabilities, vec!["ocf".to_string(), "switch".to_string()]);
    }

    #[test]
    fn builds_command_bodies() {
        assert_eq!(
            command_body("main", "thermostatCoolingSetpoint", "setCoolingSetpoint", vec![json!(24)]),
            json!({ "component": "main", "capability": "thermostatCoolingSetpoint", "command": "setCoolingSetpoint", "arguments": [24] })
        );
        assert_eq!(
            command_body("main", "switch", "on", vec![]),
            json!({ "component": "main", "capability": "switch", "command": "on" })
        );
    }

    #[test]
    fn builds_the_oauth_app_and_sign_in_page() {
        let body = oauth_app_request("thingscan-0123456789abcdef");
        assert_eq!(body["appType"], "API_ONLY");
        assert_eq!(body["classifications"], json!(["CONNECTED_SERVICE"]));
        assert_eq!(body["oauth"]["redirectUris"], json!(["https://httpbin.org/get"]));
        assert_eq!(body["oauth"]["scope"], json!(["r:devices:*", "w:devices:*", "x:devices:*", "r:locations:*"]));

        let url = reqwest::Url::parse(&authorize_url("client-1", "st8", true)).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.as_str().split('?').next(), Some(AUTHORIZE_URL));
        assert_eq!(query["client_id"], "client-1");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["redirect_uri"], REDIRECT_URI);
        assert_eq!(query["state"], "st8");
        assert_eq!(query["scope"], "r:devices:* w:devices:* x:devices:* r:locations:*");
        assert!(!authorize_url("client-1", "st8", false).contains("scope"));
    }

    #[test]
    fn reads_token_replies() {
        // Shape of a real reply (SmartThings community, May 2025).
        let ok = json!({ "token_type": "bearer", "scope": "r:devices:* x:devices:*", "refresh_token": "r-2",
            "access_tier": 0, "installed_app_id": "x", "access_token": "a-2", "expires_in": 86399 });
        assert_eq!(
            parse_grant(200, &ok),
            Ok(Grant { access_token: "a-2".into(), refresh_token: "r-2".into(), expires_in: 86399 })
        );
        let used =
            json!({ "error": "invalid_grant", "error_description": "Invalid refresh token requested by clientId: c" });
        assert_eq!(
            parse_grant(400, &used),
            Err(CloudError::Grant("Invalid refresh token requested by clientId: c".into()))
        );
        assert!(matches!(parse_grant(401, &Value::Null), Err(CloudError::Grant(_))));
        assert!(matches!(parse_grant(500, &Value::Null), Err(CloudError::Api { status: 500, .. })));
        assert!(matches!(parse_grant(200, &json!({ "access_token": "a" })), Err(CloudError::Api { .. })));
    }

    #[test]
    fn reads_api_errors_and_keeps_ids_out_of_paths() {
        let body = json!({ "requestId": "1", "error": { "code": "ConstraintViolationError", "message": "Remote control is off" } });
        assert_eq!(error_message(&body, 422), "Remote control is off");
        assert_eq!(error_message(&Value::Null, 500), "HTTP 500");
        assert_eq!(encode("d424e86b-15cf/../x?y"), "d424e86b-15cfxy");
    }
}
