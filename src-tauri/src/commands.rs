//! Commands called from the UI (see src/api.ts).

use crate::account::Account;
use crate::store::{SavedDevice, Store, StoreError};
use crate::vault::{AccountStatus, Vault};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use thingscan_core::cloud::{self, Client, CloudDevice, CloudError, OAuthApp};
use thingscan_core::reach::{self, Report, Target};
use thingscan_core::scan::{self, Event};
use thingscan_core::{Device, Endpoint, tv};

/// How long the "Add device" scan listens. Wi-Fi gets longer: macOS takes ~15s to list networks.
const SCAN_TIME: Duration = Duration::from_secs(10);
const WIFI_SCAN_TIME: Duration = Duration::from_secs(20);

/// Tells every window (the main one and the menu bar panel) that saved devices changed.
fn changed(app: &AppHandle) {
    let _ = app.emit("devices-changed", ());
}

/// The latest reachability result per device, so a window opened later (the menu bar
/// panel) shows statuses straight away.
#[derive(Default)]
pub struct Reports {
    latest: Mutex<HashMap<String, Report>>,
    /// When the last full check finished, in ms since the epoch.
    checked_at: Mutex<Option<u64>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportsSnapshot {
    reports: Vec<Report>,
    checked_at: Option<u64>,
}

#[tauri::command]
pub fn last_reports(reports: State<'_, Reports>) -> ReportsSnapshot {
    ReportsSnapshot {
        reports: reports.latest.lock().unwrap().values().cloned().collect(),
        checked_at: *reports.checked_at.lock().unwrap(),
    }
}

/// Shows the main window, e.g. from the menu bar panel, optionally on a view ("add").
#[tauri::command]
pub fn open_main(app: AppHandle, view: Option<String>) {
    crate::tray::show_main(&app, view.as_deref());
}

/// The scan running for the "Add device" screen, if any.
#[derive(Default)]
pub struct ScanTask(Mutex<Option<JoinHandle<()>>>);

#[tauri::command]
pub fn list_devices(store: State<'_, Store>) -> Vec<SavedDevice> {
    store.list()
}

/// Adds a scanned device, linking it to its SmartThings device when the account has it.
#[tauri::command]
pub fn add_device(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
    device: Device,
    name: String,
    room: Option<String>,
) -> Result<SavedDevice, StoreError> {
    let saved = store.add(device, name, room)?;
    store.auto_link(&account.devices())?;
    changed(&app);
    store.get(&saved.id)
}

#[tauri::command]
pub fn update_device(
    app: AppHandle,
    store: State<'_, Store>,
    id: String,
    name: String,
    room: Option<String>,
) -> Result<SavedDevice, StoreError> {
    let saved = store.update(&id, name, room)?;
    changed(&app);
    Ok(saved)
}

/// Renames a room for every device in it; `to` None takes them out of any room.
#[tauri::command]
pub fn rename_room(
    app: AppHandle,
    store: State<'_, Store>,
    from: String,
    to: Option<String>,
) -> Result<Vec<SavedDevice>, StoreError> {
    let moved = store.rename_room(&from, to)?;
    changed(&app);
    Ok(moved)
}

#[tauri::command]
pub fn remove_device(app: AppHandle, store: State<'_, Store>, id: String) -> Result<(), StoreError> {
    store.remove(&id)?;
    changed(&app);
    Ok(())
}

/// Starts scanning and streams progress to `on_event`. Starting again, or `cancel_scan`,
/// stops the previous scan.
#[tauri::command]
pub fn start_scan(task: State<'_, ScanTask>, on_event: Channel<Event>) {
    let options = scan::Options { timeout: SCAN_TIME, wifi_timeout: WIFI_SCAN_TIME, ..Default::default() };
    let handle = tauri::async_runtime::spawn(async move {
        scan::run(options, |event| {
            let _ = on_event.send(event);
        })
        .await;
    });
    if let Some(previous) = task.0.lock().unwrap().replace(handle) {
        previous.abort();
    }
}

#[tauri::command]
pub fn cancel_scan(task: State<'_, ScanTask>) {
    if let Some(handle) = task.0.lock().unwrap().take() {
        handle.abort();
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckUpdate {
    report: Report,
    /// The saved device after recording the result (last seen, new address).
    device: Option<SavedDevice>,
}

/// Checks that saved devices answer (all of them, or just `ids`), streaming each result
/// as it is known. Resolves once every device has a result.
#[tauri::command]
pub async fn check_devices(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
    reports: State<'_, Reports>,
    ids: Option<Vec<String>>,
    on_update: Channel<CheckUpdate>,
) -> Result<(), StoreError> {
    let targets: Vec<Target> = store
        .list()
        .into_iter()
        .filter(|d| ids.as_ref().is_none_or(|ids| ids.contains(&d.id)))
        .map(|d| Target { id: d.id, ids: d.ids, endpoints: d.endpoints })
        .collect();
    let full = ids.is_none();
    let mut failure = None;
    let _ = account.renew(None).await;
    reach::check(targets, account.client(), |report| {
        let device = store.record(&report).unwrap_or_else(|e| {
            failure.get_or_insert(e);
            None
        });
        reports.latest.lock().unwrap().insert(report.id.clone(), report.clone());
        let update = CheckUpdate { report, device };
        // Other windows hear it too; the caller gets it on its channel.
        let _ = app.emit("device-checked", &update);
        let _ = on_update.send(update);
    })
    .await;
    if full {
        *reports.checked_at.lock().unwrap() = Some(crate::store::now() * 1000);
    }
    failure.map_or(Ok(()), Err)
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    kind: &'static str,
    message: String,
}

impl From<StoreError> for CommandError {
    fn from(e: StoreError) -> Self {
        Self { kind: "store", message: e.to_string() }
    }
}

impl From<CloudError> for CommandError {
    fn from(e: CloudError) -> Self {
        let kind = if matches!(e, CloudError::Unauthorized | CloudError::Grant(_)) { "unauthorized" } else { "cloud" };
        Self { kind, message: e.to_string() }
    }
}

fn tv_error(e: anyhow::Error) -> CommandError {
    CommandError { kind: "tv", message: format!("{e:#}") }
}

fn not_connected() -> CommandError {
    CommandError { kind: "notConnected", message: "SmartThings isn't connected".into() }
}

// SmartThings account

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    connected: bool,
    /// When the token was saved or last renewed (seconds).
    saved_at: Option<u64>,
    /// The token's last four characters, shown masked in the account menu.
    token_hint: Option<String>,
    /// The token renews itself; otherwise it's a personal token that lasts 24 hours.
    renews: bool,
    /// When a renewing token's access token runs out (seconds).
    expires_at: Option<u64>,
    /// A SmartThings app is set up, so signing in through it needs no personal token.
    has_app: bool,
    devices: Vec<CloudDevice>,
    /// Saved devices that were just linked to their SmartThings device.
    linked: Vec<SavedDevice>,
    /// Why the device list couldn't be loaded, e.g. an expired token.
    error: Option<String>,
}

impl AccountState {
    fn of(account: &Account, devices: Vec<CloudDevice>, linked: Vec<SavedDevice>, error: Option<String>) -> Self {
        Self {
            connected: account.token().is_some(),
            saved_at: account.saved_at(),
            token_hint: account.token_hint(),
            renews: account.renews(),
            expires_at: account.expires_at(),
            has_app: account.app().is_some(),
            devices,
            linked,
            error: error.or_else(|| account.renew_error()),
        }
    }
}

/// Runs `call` with the SmartThings client, renewing a renewing token first when it's due. If
/// SmartThings turns the token down anyway (another device renewed, which retires the old
/// one), the newer token comes from account sync or a renewal here, and `call` runs again.
async fn with_cloud<T, F, Fut>(app: &AppHandle, call: F) -> Result<T, CloudError>
where
    F: Fn(Client) -> Fut,
    Fut: Future<Output = Result<T, CloudError>>,
{
    let account = app.state::<Account>();
    let _ = account.renew(None).await;
    let refused = account.token();
    match call(account.client().ok_or(CloudError::Unauthorized)?).await {
        Err(CloudError::Unauthorized) if account.renews() => {
            let vault = app.state::<Vault>();
            if vault.signed_in() {
                vault.sync(app).await;
            }
            if !account.renew(refused.as_deref()).await? {
                return Err(CloudError::Unauthorized);
            }
            call(account.client().ok_or(CloudError::Unauthorized)?).await
        }
        other => other,
    }
}

/// Loads the account's devices and links saved devices to them by OCF id.
async fn refresh(app: &AppHandle) -> Result<AccountState, CloudError> {
    let devices = with_cloud(app, |client| async move {
        let mut devices = client.devices().await?;
        client.fill_ocf_ids(&mut devices).await;
        Ok(devices)
    })
    .await?;
    let account = app.state::<Account>();
    account.set_devices(devices.clone());
    let linked = app.state::<Store>().auto_link(&devices).unwrap_or_default();
    Ok(AccountState::of(&account, devices, linked, None))
}

#[tauri::command]
pub async fn cloud_status(app: AppHandle, account: State<'_, Account>) -> Result<AccountState, CommandError> {
    if account.client().is_none() {
        return Ok(AccountState::of(&account, vec![], vec![], None));
    }
    let state = refresh(&app).await;
    if state.as_ref().is_ok_and(|s| !s.linked.is_empty()) {
        changed(&app);
    }
    Ok(state.unwrap_or_else(|e| AccountState::of(&account, account.devices(), vec![], Some(e.to_string()))))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connected {
    account: AccountState,
    /// Why Thingscan's SmartThings app couldn't be made, e.g. the token lacks the Apps scope.
    app_error: Option<String>,
}

/// Checks a personal token and keeps it, so appliances work straight away. Then, once, makes
/// Thingscan's SmartThings app with it: signing in through that app swaps the 24-hour token for
/// one that renews itself.
#[tauri::command]
pub async fn cloud_connect(
    app: AppHandle,
    account: State<'_, Account>,
    token: String,
) -> Result<Connected, CommandError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(CommandError { kind: "cloud", message: "Paste a token first".into() });
    }
    let client = Client::new(token);
    client.devices().await?;
    account.save(token)?;
    let app_error = match account.app() {
        Some(_) => None,
        None => match client.create_oauth_app().await {
            Ok(oauth) => {
                account.save_app(oauth, true)?;
                None
            }
            Err(CloudError::Unauthorized | CloudError::Forbidden(_)) => Some(
                "This token can't set up Thingscan's SmartThings app, so it lasts 24 hours. Generate one with Apps ticked too, and Thingscan won't need another.".into(),
            ),
            Err(e) => Some(format!("Couldn't set up Thingscan's SmartThings app ({e}), so this token lasts 24 hours.")),
        },
    };
    let state = refresh(&app).await?;
    changed(&app);
    Ok(Connected { account: state, app_error })
}

/// Keeps the client ID and secret of an OAuth-In app the user made (`smartthings apps:create`).
#[tauri::command]
pub fn cloud_set_app(
    account: State<'_, Account>,
    client_id: String,
    client_secret: String,
) -> Result<(), CommandError> {
    let (client_id, client_secret) = (client_id.trim().to_string(), client_secret.trim().to_string());
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(CommandError { kind: "cloud", message: "Enter both the client ID and the client secret".into() });
    }
    Ok(account.save_app(OAuthApp { client_id, client_secret }, false)?)
}

const SIGN_IN_WINDOW: &str = "smartthings-sign-in";
const SIGN_IN_WAIT: Duration = Duration::from_secs(15 * 60);

/// Opens SmartThings' sign-in page in a window of its own, waits for the user to allow
/// Thingscan's app, and keeps the token that renews itself.
#[tauri::command]
pub async fn cloud_sign_in(app: AppHandle, account: State<'_, Account>) -> Result<AccountState, CommandError> {
    let saved = account.app().ok_or_else(|| CommandError {
        kind: "cloud",
        message: "Set up Thingscan's SmartThings app first: connect with a personal token that has Apps ticked".into(),
    })?;
    let state = uuid::Uuid::new_v4().simple().to_string();
    let page = cloud::authorize_url(&saved.app.client_id, &state, saved.made_here);
    let page = page.parse().map_err(|_| CommandError { kind: "cloud", message: "bad sign-in address".into() })?;
    // One sign-in at a time; an earlier window's wait ends as cancelled when it closes.
    for (label, open) in app.webview_windows() {
        if label.starts_with(SIGN_IN_WINDOW) {
            let _ = open.close();
        }
    }
    let label = format!("{SIGN_IN_WINDOW}-{}", &state[..8]);
    let (send, outcome) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let send = Arc::new(Mutex::new(Some(send)));
    let answer = send.clone();
    let window = WebviewWindowBuilder::new(&app, label, WebviewUrl::External(page))
        .title("Sign in to SmartThings")
        .inner_size(480.0, 760.0)
        .on_navigation(move |url| {
            let Some(result) = sign_in_result(url, &state) else { return true };
            if let Some(send) = answer.lock().unwrap().take() {
                let _ = send.send(result);
            }
            // The code is all Thingscan needs; the page it was headed for never loads.
            false
        })
        .build()
        .map_err(|e| CommandError { kind: "cloud", message: format!("couldn't open the sign-in window: {e}") })?;
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event
            && let Some(send) = send.lock().unwrap().take()
        {
            let _ = send.send(Err("Sign-in cancelled".into()));
        }
    });
    let result = tokio::time::timeout(SIGN_IN_WAIT, outcome).await;
    let _ = window.close();
    let code = match result {
        Ok(Ok(Ok(code))) => code,
        Ok(Ok(Err(message))) => return Err(CommandError { kind: "cancelled", message }),
        _ => return Err(CommandError { kind: "cancelled", message: "Sign-in cancelled".into() }),
    };
    let grant = cloud::exchange_code(&saved.app, &code).await?;
    account.save_grant(&saved.app, grant)?;
    // Every window hears it, even if the dialog that asked was closed meanwhile.
    let _ = app.emit("smartthings-changed", ());
    let state = refresh(&app).await?;
    changed(&app);
    Ok(state)
}

/// What the sign-in page sent back, once it heads for the redirect: the code, or why not.
fn sign_in_result(url: &tauri::Url, state: &str) -> Option<Result<String, String>> {
    let redirect = tauri::Url::parse(cloud::REDIRECT_URI).ok()?;
    if url.host_str() != redirect.host_str() || url.path() != redirect.path() {
        return None;
    }
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    Some(match (query.get("code"), query.get("state")) {
        (Some(code), Some(given)) if given == state && !code.is_empty() => Ok(code.clone()),
        (Some(_), _) => Err("The sign-in didn't come from this window; try again".into()),
        (None, _) => Err(query.get("error_description").or_else(|| query.get("error")).map_or_else(
            || "SmartThings didn't sign you in".into(),
            |why| format!("SmartThings didn't sign you in: {why}"),
        )),
    })
}

/// The saved token itself, for when the user asks to see or copy it.
#[tauri::command]
pub fn cloud_token(account: State<'_, Account>) -> Result<String, CommandError> {
    account.token().ok_or_else(not_connected)
}

#[tauri::command]
pub fn cloud_disconnect(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
) -> Result<(), CommandError> {
    account.forget()?;
    store.unlink_all()?;
    changed(&app);
    Ok(())
}

/// Links a saved device to a SmartThings device (or unlinks it when `cloud_id` is None).
#[tauri::command]
pub fn cloud_link(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
    id: String,
    cloud_id: Option<String>,
) -> Result<SavedDevice, CommandError> {
    let cloud = match cloud_id {
        Some(cloud_id) => Some(account.device(&cloud_id).ok_or_else(not_connected)?),
        None => None,
    };
    let saved = store.link_cloud(&id, cloud.as_ref())?;
    changed(&app);
    Ok(saved)
}

#[tauri::command]
pub fn cloud_add(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
    cloud_id: String,
    name: String,
    room: Option<String>,
) -> Result<SavedDevice, CommandError> {
    let cloud = account.device(&cloud_id).ok_or_else(not_connected)?;
    let saved = store.add_cloud(&cloud, name, room)?;
    changed(&app);
    Ok(saved)
}

/// The SmartThings device a saved device is linked to.
fn linked_id(store: &Store, account: &Account, id: &str) -> Result<String, CommandError> {
    account.client().ok_or_else(not_connected)?;
    store.get(id)?.cloud_id.ok_or_else(|| CommandError {
        kind: "notLinked",
        message: "This device isn't linked to a SmartThings device".into(),
    })
}

/// The device's current SmartThings status (`{components: {main: {capability: {attribute: {value}}}}}`).
#[tauri::command]
pub async fn cloud_device_status(
    app: AppHandle,
    store: State<'_, Store>,
    account: State<'_, Account>,
    id: String,
) -> Result<Value, CommandError> {
    let cloud_id = linked_id(&store, &account, &id)?;
    Ok(with_cloud(&app, |client| {
        let cloud_id = cloud_id.clone();
        async move { client.status(&cloud_id).await }
    })
    .await?)
}

/// Runs one SmartThings command, then returns the status once the device has had a moment
/// to apply it.
#[tauri::command]
pub async fn cloud_command(
    app: AppHandle,
    id: String,
    component: Option<String>,
    capability: String,
    command: String,
    arguments: Vec<Value>,
) -> Result<Value, CommandError> {
    let cloud_id = linked_id(&app.state::<Store>(), &app.state::<Account>(), &id)?;
    let component = component.unwrap_or_else(|| "main".into());
    with_cloud(&app, |client| {
        let (cloud_id, component, arguments) = (cloud_id.clone(), component.clone(), arguments.clone());
        let (capability, command) = (capability.clone(), command.clone());
        async move { client.command(&cloud_id, &component, &capability, &command, arguments).await }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    Ok(with_cloud(&app, |client| {
        let cloud_id = cloud_id.clone();
        async move { client.status(&cloud_id).await }
    })
    .await?)
}

// Samsung TV remote

/// Open remote connections, by saved device id.
#[derive(Default)]
pub struct TvSessions(tokio::sync::Mutex<HashMap<String, tv::Remote>>);

/// How long the first connection waits for someone to choose Allow on the TV.
const TV_ALLOW_WAIT: Duration = Duration::from_secs(30);
/// Reconnecting with a saved token needs no prompt.
const TV_RECONNECT_WAIT: Duration = Duration::from_secs(8);
const TV_CLIENT_NAME: &str = "Thingscan";

fn tv_ip(device: &SavedDevice) -> Option<IpAddr> {
    device
        .endpoints
        .iter()
        .find_map(|e| match e {
            Endpoint::SamsungTv { ip, .. } | Endpoint::Ocf { ip, .. } => Some(*ip),
            Endpoint::Upnp { location, .. } => {
                let host = thingscan_core::device::url_host(location);
                host.rsplit_once(':').map_or(host, |(h, _)| h).parse().ok()
            }
            _ => None,
        })
        .or_else(|| device.last_address.as_deref()?.split(':').next()?.parse().ok())
}

fn tv_mac(device: &SavedDevice) -> Option<String> {
    device
        .endpoints
        .iter()
        .find_map(|e| match e {
            Endpoint::SamsungTv { mac: Some(mac), .. } => Some(mac.clone()),
            _ => None,
        })
        .or_else(|| device.details.iter().find(|(k, _)| k.ends_with("tv.wifi_mac")).map(|(_, v)| v.clone()))
}

async fn open_remote(store: &Store, sessions: &TvSessions, id: &str, wait: Duration) -> Result<(), CommandError> {
    let device = store.get(id)?;
    let ip = tv_ip(&device)
        .ok_or_else(|| CommandError { kind: "tv", message: "No network address is known for this TV".into() })?;
    let connected = tv::connect(ip, TV_CLIENT_NAME, device.tv_token.as_deref(), wait).await.map_err(tv_error)?;
    if let Some(token) = connected.token.filter(|t| device.tv_token.as_ref() != Some(t)) {
        store.set_tv_token(id, token)?;
    }
    sessions.0.lock().await.insert(id.to_string(), connected.remote);
    // While the TV is on, note its MAC so "Turn on TV" works after it goes to standby.
    if tv_mac(&device).is_none() {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap_or_default();
        if let Some(mac) = thingscan_core::ssdp::tv_wifi_mac(&http, ip).await {
            store.set_tv_mac(id, mac)?;
        }
    }
    Ok(())
}

/// Opens the remote. The first time, the TV asks whether to allow Thingscan.
#[tauri::command]
pub async fn tv_connect(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
) -> Result<(), CommandError> {
    if sessions.0.lock().await.get(&id).is_some_and(tv::Remote::is_open) {
        return Ok(());
    }
    open_remote(&store, &sessions, &id, TV_ALLOW_WAIT).await
}

#[tauri::command]
pub async fn tv_disconnect(sessions: State<'_, TvSessions>, id: String) -> Result<(), CommandError> {
    sessions.0.lock().await.remove(&id);
    Ok(())
}

enum TvAction {
    Click(String),
    Press(String),
    Release(String),
    Launch(String),
    Pointer(i32, i32),
    PointerClick,
}

/// Sends on the open connection, reconnecting once if the TV dropped it.
async fn tv_send(store: &Store, sessions: &TvSessions, id: &str, action: TvAction) -> Result<(), CommandError> {
    for attempt in 0..2 {
        let needs_connection = !sessions.0.lock().await.get(id).is_some_and(tv::Remote::is_open);
        if needs_connection {
            open_remote(store, sessions, id, TV_RECONNECT_WAIT).await?;
        }
        let mut open = sessions.0.lock().await;
        let Some(remote) = open.get_mut(id) else { continue };
        let result = match &action {
            TvAction::Click(key) => remote.click(key).await,
            TvAction::Press(key) => remote.press(key).await,
            TvAction::Release(key) => remote.release(key).await,
            TvAction::Launch(app) => remote.launch(app).await,
            TvAction::Pointer(dx, dy) => remote.move_pointer(*dx, *dy).await,
            TvAction::PointerClick => remote.pointer_click().await,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) if attempt == 1 => return Err(tv_error(e)),
            // The socket may have died without us noticing; reconnect and try once more.
            Err(_) => {
                open.remove(id);
            }
        }
    }
    Err(CommandError { kind: "tv", message: "Lost the connection to the TV".into() })
}

#[tauri::command]
pub async fn tv_key(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
    key: String,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::Click(key)).await
}

/// Puts a key down; the remote sends it again every so often while it's held.
#[tauri::command]
pub async fn tv_press(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
    key: String,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::Press(key)).await
}

#[tauri::command]
pub async fn tv_release(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
    key: String,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::Release(key)).await
}

#[tauri::command]
pub async fn tv_launch(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
    app_id: String,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::Launch(app_id)).await
}

/// Moves the TV's pointer from the touchpad, by at most 200 points per step.
#[tauri::command]
pub async fn tv_pointer(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
    dx: i32,
    dy: i32,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::Pointer(dx.clamp(-200, 200), dy.clamp(-200, 200))).await
}

#[tauri::command]
pub async fn tv_pointer_click(
    store: State<'_, Store>,
    sessions: State<'_, TvSessions>,
    id: String,
) -> Result<(), CommandError> {
    tv_send(&store, &sessions, &id, TvAction::PointerClick).await
}

/// Turns the TV on when its remote service doesn't answer: Wake-on-LAN, and SmartThings'
/// `switch on` when it's linked (a TV in standby stays connected to SmartThings). Fails only
/// when neither went out.
#[tauri::command]
pub async fn tv_wake(app: AppHandle, store: State<'_, Store>, id: String) -> Result<(), CommandError> {
    let device = store.get(&id)?;
    let cloud_id = device.cloud_id.clone().filter(|_| app.state::<Account>().client().is_some());
    let cloud = async {
        let cloud_id = cloud_id?;
        Some(
            with_cloud(&app, |client| {
                let cloud_id = cloud_id.clone();
                async move { client.command(&cloud_id, "main", "switch", "on", vec![]).await }
            })
            .await,
        )
    };
    match tokio::join!(wake_on_lan(&store, &id, &device), cloud) {
        (Ok(()), _) | (_, Some(Ok(()))) => Ok(()),
        (Err(lan), None) => Err(lan),
        (Err(lan), Some(Err(cloud))) => {
            Err(CommandError { kind: "tv", message: format!("{} SmartThings couldn't either: {cloud}", lan.message) })
        }
    }
}

async fn wake_on_lan(store: &Store, id: &str, device: &SavedDevice) -> Result<(), CommandError> {
    let mac = match tv_mac(device) {
        Some(mac) => mac,
        None => {
            // The system may still know it from when the TV was last on.
            let ip = tv_ip(device);
            let mac = match ip {
                Some(ip) => tv::arp_mac(ip).await,
                None => None,
            };
            let mac = mac.ok_or_else(|| CommandError {
                kind: "tv",
                message: "This TV's MAC address isn't known yet. Turn it on by hand once and open the remote; after that Thingscan can wake it.".into(),
            })?;
            store.set_tv_mac(id, mac.clone())?;
            mac
        }
    };
    tv::wake(&mac).await.map_err(tv_error)
}

// iPhone sync

#[tauri::command]
pub fn sync_status(sync: State<'_, crate::sync::Sync>) -> crate::sync::SyncStatus {
    sync.status()
}

/// Shows a pairing code for the iPhone app, valid for five minutes.
#[tauri::command]
pub fn sync_start_pairing(sync: State<'_, crate::sync::Sync>) -> Result<crate::sync::SyncStatus, StoreError> {
    sync.start_pairing()?;
    Ok(sync.status())
}

#[tauri::command]
pub fn sync_cancel_pairing(sync: State<'_, crate::sync::Sync>) -> crate::sync::SyncStatus {
    sync.cancel_pairing();
    sync.status()
}

#[tauri::command]
pub fn sync_remove_phone(
    sync: State<'_, crate::sync::Sync>,
    id: String,
) -> Result<crate::sync::SyncStatus, StoreError> {
    sync.remove_phone(&id)?;
    Ok(sync.status())
}

// Thingscan account

impl From<crate::vault::VaultError> for CommandError {
    fn from(e: crate::vault::VaultError) -> Self {
        Self { kind: "account", message: e.0 }
    }
}

#[tauri::command]
pub fn account_status(vault: State<'_, Vault>) -> AccountStatus {
    vault.status()
}

/// Creates an account on `server` and uploads this Mac's devices to it.
#[tauri::command]
pub async fn account_sign_up(
    app: AppHandle,
    vault: State<'_, Vault>,
    server: String,
    email: String,
    password: String,
) -> Result<AccountStatus, CommandError> {
    Ok(vault.sign_up(&app, &server, &email, &password).await?)
}

/// Signs in and merges the account's devices with this Mac's.
#[tauri::command]
pub async fn account_sign_in(
    app: AppHandle,
    vault: State<'_, Vault>,
    server: String,
    email: String,
    password: String,
) -> Result<AccountStatus, CommandError> {
    Ok(vault.sign_in(&app, &server, &email, &password).await?)
}

#[tauri::command]
pub async fn account_sign_out(app: AppHandle, vault: State<'_, Vault>) -> Result<AccountStatus, CommandError> {
    Ok(vault.sign_out(&app).await)
}

#[tauri::command]
pub async fn account_sync_now(app: AppHandle, vault: State<'_, Vault>) -> Result<AccountStatus, CommandError> {
    Ok(vault.sync(&app).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_code_only_from_the_redirect_with_our_state() {
        let at = |url: &str| sign_in_result(&tauri::Url::parse(url).unwrap(), "st8");
        assert_eq!(at("https://account.samsung.com/accounts/v1/signin"), None);
        assert_eq!(at("https://httpbin.org/anything?code=X1"), None);
        assert_eq!(at("https://httpbin.org/get?code=X1y2Z3&state=st8"), Some(Ok("X1y2Z3".into())));
        assert!(matches!(at("https://httpbin.org/get?code=X1y2Z3&state=other"), Some(Err(_))));
        assert_eq!(
            at("https://httpbin.org/get?error=access_denied&error_description=User%20denied&state=st8"),
            Some(Err("SmartThings didn't sign you in: User denied".into()))
        );
    }
}
