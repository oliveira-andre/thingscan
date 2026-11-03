// Typed wrappers around the Rust commands registered in src-tauri/src/lib.rs.
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Kind =
  | "ready_to_add"
  | "hub"
  | "tv"
  | "appliance"
  | "matter"
  | "thread_border_router"
  | "tracker"
  | "samsung"
  | "other";

/** Where a finding came from. */
export type Source = "ocf" | "ssdp" | "mdns" | "ble" | "softap";

/** A discovery channel of the "Add device" scan. */
export type ScanChannel = "ocf" | "ssdp" | "mdns" | "ble" | "wifi";

export type Endpoint =
  | { type: "ocf"; ip: string; di: string | null }
  | { type: "upnp"; location: string; udn: string | null }
  | { type: "samsungTv"; ip: string; id: string | null; mac?: string | null }
  | { type: "mdns"; service: string; fullname: string }
  | { type: "ble"; id: string }
  | { type: "softAp"; ssid: string }
  | { type: "cloud"; deviceId: string };

/** A device found by a scan, merged across channels. */
export interface Device {
  key: string;
  kind: Kind;
  name: string;
  address: string;
  sources: Source[];
  manufacturer?: string;
  model?: string;
  ids: string[];
  endpoints: Endpoint[];
  details: Record<string, string>;
}

/** A device the user added to their home. */
export interface SavedDevice {
  id: string;
  name: string;
  room: string | null;
  kind: Kind;
  manufacturer: string | null;
  model: string | null;
  ids: string[];
  endpoints: Endpoint[];
  sources: Source[];
  details: Record<string, string>;
  /** Seconds since the epoch. */
  addedAt: number;
  lastSeen: number | null;
  lastAddress: string | null;
  /** The linked device in the user's SmartThings account. */
  cloudId: string | null;
  /** Set once a Samsung TV has allowed this remote. */
  tvToken: string | null;
}

export type ScanEvent =
  | { event: "started"; channel: ScanChannel }
  | { event: "devices"; devices: Device[] }
  | { event: "done"; channel: ScanChannel; found: number; error: string | null }
  | { event: "finished"; devices: Device[] };

export type Status = "reachable" | "unreachable" | "unknown";

export interface Probe {
  /** "OCF", "UPnP", "TV API", "mDNS", "BLE" or "Wi-Fi". */
  via: string;
  target: string;
  status: Status;
  detail: string;
  latencyMs: number | null;
}

export interface Report {
  id: string;
  status: Status;
  address: string | null;
  probes: Probe[];
  relocated: Endpoint[] | null;
}

export interface CheckUpdate {
  report: Report;
  device: SavedDevice | null;
}

export function errorMessage(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}

export const listDevices = () => invoke<SavedDevice[]>("list_devices");

export const addDevice = (device: Device, name: string, room: string | null) =>
  invoke<SavedDevice>("add_device", { device, name, room });

export const updateDevice = (id: string, name: string, room: string | null) =>
  invoke<SavedDevice>("update_device", { id, name, room });

export const renameRoom = (from: string, to: string | null) => invoke<SavedDevice[]>("rename_room", { from, to });

export const removeDevice = (id: string) => invoke<void>("remove_device", { id });

export function startScan(onEvent: (event: ScanEvent) => void): Promise<void> {
  const channel = new Channel<ScanEvent>();
  channel.onmessage = onEvent;
  return invoke("start_scan", { onEvent: channel });
}

export const cancelScan = () => invoke<void>("cancel_scan");

/** Resolves once every device (or each of `ids`) has a result. */
export function checkDevices(ids: string[] | null, onUpdate: (update: CheckUpdate) => void): Promise<void> {
  const channel = new Channel<CheckUpdate>();
  channel.onmessage = onUpdate;
  return invoke("check_devices", { ids, onUpdate: channel });
}

// SmartThings account

export interface CloudDevice {
  deviceId: string;
  name: string;
  manufacturer: string | null;
  /** e.g. "AirConditioner", "Washer", "Television". */
  category: string | null;
  ocfType: string | null;
  model: string | null;
  capabilities: string[];
  di: string | null;
}

export interface AccountState {
  connected: boolean;
  /** When the token was saved or last renewed, in seconds since the epoch. */
  savedAt: number | null;
  /** The token's last four characters. */
  tokenHint: string | null;
  /** The token renews itself; otherwise it's a personal token that lasts 24 hours. */
  renews: boolean;
  /** When a renewing token's access token runs out, in seconds since the epoch. */
  expiresAt: number | null;
  /** Thingscan's SmartThings app is set up, so signing in through it needs no personal token. */
  hasApp: boolean;
  devices: CloudDevice[];
  linked: SavedDevice[];
  error: string | null;
}

/** `{ components: { main: { <capability>: { <attribute>: { value, unit } } } } }` */
export interface CloudStatus {
  components?: Record<string, Record<string, Record<string, { value: unknown; unit?: string }>>>;
}

export function errorKind(error: unknown): string | null {
  return error && typeof error === "object" && "kind" in error ? String((error as { kind: unknown }).kind) : null;
}

export const cloudStatus = () => invoke<AccountState>("cloud_status");
/** Keeps a personal token and, once, sets up Thingscan's SmartThings app with it. */
export const cloudConnect = (token: string) =>
  invoke<{ account: AccountState; appError: string | null }>("cloud_connect", { token });
/** Keeps an OAuth-In app the user made with the SmartThings CLI. */
export const cloudSetApp = (clientId: string, clientSecret: string) =>
  invoke<void>("cloud_set_app", { clientId, clientSecret });
/** Opens SmartThings' sign-in window; resolves once the user allowed Thingscan's app. */
export const cloudSignIn = () => invoke<AccountState>("cloud_sign_in");
export const cloudDisconnect = () => invoke<void>("cloud_disconnect");
/** The saved token itself; only asked for when the user reveals or copies it. */
export const cloudToken = () => invoke<string>("cloud_token");
export const cloudLink = (id: string, cloudId: string | null) => invoke<SavedDevice>("cloud_link", { id, cloudId });
export const cloudAdd = (cloudId: string, name: string, room: string | null) =>
  invoke<SavedDevice>("cloud_add", { cloudId, name, room });
export const cloudDeviceStatus = (id: string) => invoke<CloudStatus>("cloud_device_status", { id });
export const cloudCommand = (id: string, capability: string, command: string, args: unknown[] = []) =>
  invoke<CloudStatus>("cloud_command", { id, component: null, capability, command, arguments: args });

// Samsung TV remote

export const tvConnect = (id: string) => invoke<void>("tv_connect", { id });
export const tvDisconnect = (id: string) => invoke<void>("tv_disconnect", { id });
export const tvKey = (id: string, key: string) => invoke<void>("tv_key", { id, key });
export const tvPress = (id: string, key: string) => invoke<void>("tv_press", { id, key });
export const tvRelease = (id: string, key: string) => invoke<void>("tv_release", { id, key });
export const tvLaunch = (id: string, appId: string) => invoke<void>("tv_launch", { id, appId });
export const tvWake = (id: string) => invoke<void>("tv_wake", { id });
export const tvPointer = (id: string, dx: number, dy: number) => invoke<void>("tv_pointer", { id, dx, dy });
export const tvPointerClick = (id: string) => invoke<void>("tv_pointer_click", { id });

// Shared between the main window and the menu bar panel

export interface ReportsSnapshot {
  reports: Report[];
  /** When the last full check finished, in ms. */
  checkedAt: number | null;
}

export const lastReports = () => invoke<ReportsSnapshot>("last_reports");
export const openMain = (view: "home" | "add" | null = null) => invoke<void>("open_main", { view });

/** Saved devices changed in some window (added, renamed, linked, removed). */
export const onDevicesChanged = (handler: () => void) => listen("devices-changed", () => handler());
/** A reachability result, from a check started in any window. */
export const onDeviceChecked = (handler: (update: CheckUpdate) => void) =>
  listen<CheckUpdate>("device-checked", (event) => handler(event.payload));

// iPhone sync

export interface PairedPhone {
  id: string;
  name: string;
  pairedAt: number;
  lastSync: number | null;
}

export interface SyncStatus {
  macName: string;
  port: number | null;
  /** "ip:port" addresses the iPhone can reach this Mac at. */
  addresses: string[];
  phones: PairedPhone[];
  code: { code: string; expiresAt: number } | null;
}

export const syncStatus = () => invoke<SyncStatus>("sync_status");
export const syncStartPairing = () => invoke<SyncStatus>("sync_start_pairing");
export const syncCancelPairing = () => invoke<SyncStatus>("sync_cancel_pairing");
export const syncRemovePhone = (id: string) => invoke<SyncStatus>("sync_remove_phone", { id });
/** A phone paired or synced. */
export const onSyncChanged = (handler: (event: { paired?: string; synced?: string }) => void) =>
  listen<{ paired?: string; synced?: string }>("sync-changed", (event) => handler(event.payload));

// Thingscan account (end-to-end encrypted sync through a Thingscan server)

export interface ThingscanAccount {
  signedIn: boolean;
  email: string | null;
  server: string | null;
  /** Seconds since the epoch. */
  lastSync: number | null;
  syncing: boolean;
  error: string | null;
}

export const accountStatus = () => invoke<ThingscanAccount>("account_status");
export const accountSignUp = (server: string, email: string, password: string) =>
  invoke<ThingscanAccount>("account_sign_up", { server, email, password });
export const accountSignIn = (server: string, email: string, password: string) =>
  invoke<ThingscanAccount>("account_sign_in", { server, email, password });
export const accountSignOut = () => invoke<ThingscanAccount>("account_sign_out");
export const accountSyncNow = () => invoke<ThingscanAccount>("account_sync_now");
export const onAccountChanged = (handler: (status: ThingscanAccount) => void) =>
  listen<ThingscanAccount>("account-changed", (event) => handler(event.payload));
/** Account sync brought a SmartThings token (or its removal) from another device. */
export const onSmartThingsChanged = (handler: () => void) => listen("smartthings-changed", () => handler());
