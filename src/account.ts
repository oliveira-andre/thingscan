// The SmartThings account, used to control appliances. Shared by every view through state.ts.
// The account menu sits at the right of the top nav: the Thingscan account (sign in to sync
// through a Thingscan server); SmartThings status, the token (masked, with reveal and copy),
// signing in so it renews itself, replace and disconnect; and pairing the iPhone app with this
// Mac directly.
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import * as api from "./api";
import type { CloudDevice } from "./api";
import { button, el } from "./dom";
import { openAccountDialog, openConnectDialog } from "./dialog";
import { ago } from "./format";
import { icon } from "./icons";
import { loadDevices, notify, subscribe, upsertDevice } from "./state";
import { toast } from "./toast";

export const account = {
  connected: false,
  savedAt: null as number | null,
  /** The token's last four characters. */
  tokenHint: null as string | null,
  /** The token renews itself; otherwise it's a personal token that lasts 24 hours. */
  renews: false,
  /** Thingscan's SmartThings app is set up, so signing in needs no personal token. */
  hasApp: false,
  devices: [] as CloudDevice[],
  /** e.g. an expired token; the account stays connected so it can be replaced. */
  error: null as string | null,
  loading: false,
};

function apply(state: api.AccountState): void {
  account.connected = state.connected;
  account.savedAt = state.savedAt;
  account.tokenHint = state.tokenHint;
  account.renews = state.renews;
  account.hasApp = state.hasApp;
  account.devices = state.devices;
  account.error = state.error;
  for (const device of state.linked) {
    upsertDevice(device);
  }
}

/** Loads the account at startup and links saved devices to their SmartThings devices. */
export async function loadAccount(): Promise<void> {
  account.loading = true;
  notify();
  try {
    apply(await api.cloudStatus());
  } catch (error) {
    account.error = api.errorMessage(error);
  } finally {
    account.loading = false;
    notify();
  }
}

/**
 * Opens the connect dialog, or with `signIn` its step that swaps a personal token for one that
 * renews itself. Resolves true once connected.
 */
export async function connectAccount(reason?: string, signIn = false): Promise<boolean> {
  const connection = await openConnectDialog(reason, signIn);
  if (!connection) return false;
  const { state, appError } = connection;
  apply(state);
  notify();
  const linked = state.linked.length;
  const found =
    `${state.devices.length} device${state.devices.length === 1 ? "" : "s"} in your account` +
    (linked ? `, ${linked} matched to your home` : "");
  if (state.renews) {
    toast("success", "SmartThings connected", `${found}. Thingscan renews its access by itself.`);
  } else {
    toast(appError ? "info" : "success", "SmartThings connected for 24 hours", appError ?? found);
  }
  return true;
}

/** Signs in through Thingscan's SmartThings app (when it's set up), or asks for a token. */
export function renewAccess(reason?: string): Promise<boolean> {
  return connectAccount(reason, account.hasApp);
}

export async function disconnectAccount(): Promise<void> {
  try {
    await api.cloudDisconnect();
    account.connected = false;
    account.devices = [];
    account.error = null;
    account.savedAt = null;
    account.tokenHint = null;
    account.renews = false;
    // Saved devices lose their link; reload them.
    await loadDevices();
    toast("info", "SmartThings disconnected");
  } catch (error) {
    toast("error", "Couldn't disconnect", api.errorMessage(error));
  }
}

/** Call when a SmartThings request fails, so an expired token is noticed everywhere. */
export function noteCloudError(error: unknown): void {
  if (api.errorKind(error) === "unauthorized") {
    account.error = api.errorMessage(error);
    notify();
  }
}

/** Signs in to (or creates) a Thingscan account. */
async function signIn(mode: "signIn" | "signUp"): Promise<void> {
  const status = await openAccountDialog(mode);
  if (!status) return;
  const title = mode === "signUp" ? "Account created" : "Signed in";
  if (status.error) {
    toast("info", title, status.error);
  } else {
    toast("success", title, `Your devices now sync through ${serverHost(status.server)}.`);
  }
}

function serverHost(server: string | null): string {
  try {
    return server ? new URL(server).host : "";
  } catch {
    return server ?? "";
  }
}

/** The account button in the top nav, and the menu it opens. */
export function mountAccountMenu(host: HTMLElement): void {
  const trigger = el("button", "account-button", icon("user", 22), el("span", "account-badge"));
  trigger.type = "button";
  trigger.title = "Account";
  trigger.setAttribute("aria-label", "Account");
  trigger.setAttribute("aria-haspopup", "true");
  host.replaceChildren(trigger);

  let menu: HTMLElement | null = null;
  // Shown in full only after the user asks; forgotten when the menu closes.
  let revealed: string | null = null;
  let sync: api.SyncStatus | null = null;
  let thingscan: api.ThingscanAccount | null = null;
  let countdown: HTMLElement | null = null;
  let ticker: number | undefined;

  const loadSync = async () => {
    try {
      sync = await api.syncStatus();
    } catch {
      sync = null;
    }
    render();
  };
  void api.accountStatus().then((status) => {
    thingscan = status;
    render();
  });
  void api.onAccountChanged((status) => {
    thingscan = status;
    render();
  });
  // Account sync brought a SmartThings token (or a disconnect) from another device.
  void api.onSmartThingsChanged(() => void loadAccount());
  void api.onSyncChanged((event) => {
    if (event.paired) toast("success", "iPhone paired", `${event.paired} can now sync with this Mac.`);
    if (menu) void loadSync();
  });

  const renderBadge = () => {
    const badge = trigger.querySelector<HTMLElement>(".account-badge")!;
    badge.className = `account-badge ${!account.connected ? "" : account.error ? "unknown" : "reachable"}`;
    trigger.title = !account.connected
      ? "Account · SmartThings not connected"
      : account.error
        ? "Account · SmartThings token expired"
        : "Account · SmartThings connected";
  };

  const close = () => {
    menu?.remove();
    menu = null;
    revealed = null;
    window.clearInterval(ticker);
    trigger.classList.remove("open");
    document.removeEventListener("mousedown", onOutside, true);
    document.removeEventListener("keydown", onKey, true);
  };
  const onOutside = (event: MouseEvent) => {
    const target = event.target as Node;
    if (menu && !menu.contains(target) && !trigger.contains(target)) close();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      close();
    }
  };

  const render = () => {
    if (!menu) return;
    menu.replaceChildren(...menuContent());
  };

  function menuContent(): Node[] {
    const status = !account.connected
      ? el("span", "account-status", "Not connected")
      : account.error
        ? el(
            "span",
            "account-status warn",
            el("span", "status-dot unknown"),
            account.renews ? "Needs signing in again" : "Token expired",
          )
        : el(
            "span",
            "account-status",
            el("span", "status-dot reachable"),
            `Connected · ${account.devices.length} device${account.devices.length === 1 ? "" : "s"}`,
          );
    const head = el(
      "div",
      "account-head",
      el("span", "account-avatar", icon("user", 26)),
      el("div", "", el("div", "account-title", "SmartThings account"), status),
    );

    if (!account.connected) {
      return [
        thingscanSection(),
        head,
        el(
          "p",
          "account-note",
          "Connect your SmartThings account to control air conditioners, washers and other appliances.",
        ),
        button(
          [icon("link", 14), "Connect SmartThings"],
          () => {
            close();
            void connectAccount();
          },
          "button-primary account-wide",
        ),
        phoneSection(),
      ];
    }

    const masked = "•".repeat(16) + (account.tokenHint ?? "");
    const value = el("span", `token-value${revealed ? " revealed" : ""}`, revealed ?? masked);
    const reveal = button(
      icon(revealed ? "eyeOff" : "eye", 15),
      async () => {
        if (revealed) {
          revealed = null;
        } else {
          try {
            revealed = await api.cloudToken();
          } catch (error) {
            toast("error", "Couldn't read the token", api.errorMessage(error));
          }
        }
        render();
      },
      "token-action",
    );
    reveal.title = revealed ? "Hide token" : "Show token";
    const copyLabel = el("span", "", "Copy");
    const copy = button(
      [icon("copy", 14), copyLabel],
      async () => {
        try {
          const token = revealed ?? (await api.cloudToken());
          await copyText(token);
          copy.replaceChildren(icon("check", 14), "Copied");
          copy.classList.add("done");
          window.setTimeout(() => {
            copy.replaceChildren(icon("copy", 14), copyLabel);
            copy.classList.remove("done");
          }, 2000);
        } catch (error) {
          toast("error", "Couldn't copy the token", api.errorMessage(error));
        }
      },
      "token-action copy",
    );
    copy.title = "Copy token";

    return [
      thingscanSection(),
      head,
      el(
        "div",
        "account-section",
        el("span", "field-label", account.renews ? "Access token" : "Personal access token"),
        el("div", "token-row", value, reveal, copy),
        el(
          "p",
          "account-note",
          account.renews
            ? `Renews by itself${account.savedAt ? `; last renewed ${ago(account.savedAt)}` : ""}.`
            : (account.savedAt ? `Saved ${ago(account.savedAt)}. ` : "") +
                "Personal tokens made since December 2024 last 24 hours.",
        ),
        account.error ? el("p", "account-note warn", account.error) : null,
        account.renews
          ? null
          : button(
              [icon("logIn", 14), "Keep access without new tokens"],
              () => {
                close();
                void renewAccess(
                  account.hasApp
                    ? undefined
                    : "Generate a token with Apps ticked as well as Devices, so Thingscan can set up its own SmartThings app.",
                );
              },
              "button-secondary account-wide",
            ),
      ),
      el(
        "div",
        "account-actions",
        button(
          account.renews ? [icon("logIn", 15), "Sign in again"] : [icon("key", 15), "Replace token"],
          () => {
            close();
            void connectAccount(undefined, account.renews);
          },
          "menu-item",
        ),
        button(
          [icon("logOut", 15), "Disconnect"],
          () => {
            close();
            void disconnectAccount();
          },
          "menu-item danger",
        ),
      ),
      phoneSection(),
    ];
  }

  function thingscanSection(): HTMLElement {
    const label = el("span", "field-label", "Thingscan account");
    if (!thingscan?.signedIn) {
      return el(
        "div",
        "account-section",
        label,
        el(
          "p",
          "account-note",
          "Sign in to keep your devices and SmartThings token in sync with your iPhone and other Macs, wherever they are.",
        ),
        el(
          "div",
          "account-buttons",
          button(
            [icon("logIn", 14), "Sign in"],
            () => {
              close();
              void signIn("signIn");
            },
            "button-primary button-small",
          ),
          button(
            "Create account",
            () => {
              close();
              void signIn("signUp");
            },
            "button-secondary button-small",
          ),
        ),
      );
    }
    const status = thingscan;
    const state = status.syncing
      ? "Syncing…"
      : status.lastSync
        ? `Synced ${ago(status.lastSync)}`
        : "Not synced yet";
    return el(
      "div",
      "account-section",
      label,
      el(
        "div",
        "phone-row",
        icon("user", 15),
        el(
          "div",
          "phone-main",
          el("div", "phone-name", status.email ?? ""),
          el("div", "account-note", `${serverHost(status.server)} · ${state}`),
        ),
      ),
      status.error ? el("p", "account-note warn", status.error) : null,
      el(
        "div",
        "account-actions",
        button(
          [icon("refresh", 15), "Sync now"],
          async () => {
            thingscan = await api.accountSyncNow();
            render();
          },
          "menu-item",
        ),
        button(
          [icon("logOut", 15), "Sign out"],
          async () => {
            thingscan = await api.accountSignOut();
            toast("info", "Signed out", "Your devices stay on this Mac; they just stop syncing.");
            render();
          },
          "menu-item danger",
        ),
      ),
    );
  }

  function phoneSection(): HTMLElement {
    const section = el("div", "account-section", el("span", "field-label", "iPhone app"));
    if (thingscan?.signedIn) {
      section.append(
        el(
          "p",
          "account-note",
          "An iPhone signed in to your Thingscan account syncs through it. Pairing is for an iPhone without one, and only works on this Wi-Fi.",
        ),
      );
    }
    countdown = null;
    if (!sync) {
      section.append(el("p", "account-note", "Starting…"));
      return section;
    }
    const status = sync;
    if (status.code) {
      countdown = el("span", "num", remaining(status.code.expiresAt));
      section.append(
        el("div", "pair-code num", `${status.code.code.slice(0, 3)} ${status.code.code.slice(3)}`),
        el(
          "p",
          "account-note",
          `On your iPhone, open Thingscan, tap the person icon, choose Pair with Mac, pick “${status.macName}” and enter this code. Expires in `,
          countdown,
          ".",
        ),
        button(
          "Cancel",
          async () => {
            sync = await api.syncCancelPairing();
            render();
          },
          "menu-item",
        ),
      );
    } else {
      section.append(
        button(
          [icon("phone", 15), "Pair an iPhone"],
          async () => {
            try {
              sync = await api.syncStartPairing();
            } catch (error) {
              toast("error", "Couldn't start pairing", api.errorMessage(error));
            }
            render();
          },
          "menu-item",
        ),
      );
    }
    for (const phone of status.phones) {
      const remove = button(
        icon("close", 14),
        async () => {
          sync = await api.syncRemovePhone(phone.id);
          render();
        },
        "token-action",
      );
      remove.title = `Unpair ${phone.name}`;
      section.append(
        el(
          "div",
          "phone-row",
          icon("phone", 15),
          el(
            "div",
            "phone-main",
            el("div", "phone-name", phone.name),
            el("div", "account-note", phone.lastSync ? `Synced ${ago(phone.lastSync)}` : `Paired ${ago(phone.pairedAt)}, not synced yet`),
          ),
          remove,
        ),
      );
    }
    if (status.addresses.length) {
      section.append(el("p", "account-note", "This Mac: ", el("span", "mono-inline", status.addresses.join(", "))));
    }
    return section;
  }

  trigger.addEventListener("click", () => {
    if (menu) {
      close();
      return;
    }
    menu = el("div", "account-menu");
    menu.setAttribute("role", "dialog");
    menu.setAttribute("aria-label", "Account");
    document.querySelector("#overlays")!.append(menu);
    trigger.classList.add("open");
    render();
    void loadSync();
    // Counts the pairing code down without re-rendering the menu.
    ticker = window.setInterval(() => {
      if (!sync?.code || !countdown) return;
      if (sync.code.expiresAt * 1000 <= Date.now()) {
        void loadSync();
      } else {
        countdown.textContent = remaining(sync.code.expiresAt);
      }
    }, 1000);
    document.addEventListener("mousedown", onOutside, true);
    document.addEventListener("keydown", onKey, true);
  });

  subscribe(() => {
    renderBadge();
    render();
  });
  renderBadge();
}

/** Copies through Tauri's clipboard plugin, falling back to the web API outside the app. */
async function copyText(text: string): Promise<void> {
  try {
    await writeText(text);
  } catch {
    await navigator.clipboard.writeText(text);
  }
}

/** "4:32" until `expiresAt` (seconds since the epoch). */
function remaining(expiresAt: number): string {
  const left = Math.max(0, Math.round(expiresAt - Date.now() / 1000));
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}
