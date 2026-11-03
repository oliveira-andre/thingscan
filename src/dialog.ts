// Modal dialogs: adding a device, connecting SmartThings, and confirming a removal.
import * as api from "./api";
import type { CloudDevice, Device, Endpoint, Kind, SavedDevice } from "./api";
import { button, el } from "./dom";
import { categoryLabel, kindOfCategory, subtitle } from "./format";
import { icon } from "./icons";
import { rooms } from "./state";
import { kindTile } from "./widgets";

export function modal(content: HTMLElement, onDismiss: () => void): { close: () => void } {
  const backdrop = el("div", "backdrop", content);
  const close = () => {
    document.removeEventListener("keydown", onKey, true);
    backdrop.remove();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      close();
      onDismiss();
    }
  };
  backdrop.addEventListener("mousedown", (event) => {
    if (event.target === backdrop) {
      close();
      onDismiss();
    }
  });
  document.addEventListener("keydown", onKey, true);
  document.querySelector("#overlays")!.append(backdrop);
  return { close };
}

export function roomInput(value: string | null): HTMLInputElement {
  const listId = "room-options";
  document.getElementById(listId)?.remove();
  const list = el("datalist");
  list.id = listId;
  for (const room of rooms()) {
    const option = el("option");
    option.value = room;
    list.append(option);
  }
  document.body.append(list);
  const input = el("input", "input");
  input.setAttribute("list", listId);
  input.placeholder = "e.g. Living room";
  input.value = value ?? "";
  return input;
}

const CHECK_LABELS: Record<Endpoint["type"], string> = {
  ocf: "OCF",
  upnp: "UPnP",
  samsungTv: "its TV API",
  mdns: "mDNS",
  ble: "Bluetooth",
  softAp: "Wi-Fi",
  cloud: "SmartThings",
};

function howChecked(endpoints: Endpoint[]): string {
  const ways = [...new Set(endpoints.filter((e) => e.type !== "softAp").map((e) => CHECK_LABELS[e.type]))];
  if (ways.length === 0) {
    return "It can't be checked automatically, since it is only visible as a setup network.";
  }
  const list = ways.length === 1 ? ways[0] : `${ways.slice(0, -1).join(", ")} and ${ways[ways.length - 1]}`;
  return `Each time Thingscan opens it checks this device over ${list}.`;
}

interface AddOptions {
  kind: Kind;
  name: string;
  details: Record<string, string>;
  subtitle: string;
  notes: HTMLElement[];
  save: (name: string, room: string | null) => Promise<SavedDevice>;
}

/** Asks for a name and room, then saves. Resolves to null if cancelled. */
function nameAndRoom(options: AddOptions): Promise<SavedDevice | null> {
  return new Promise((resolve) => {
    const name = el("input", "input");
    name.value = options.name;
    const room = roomInput(null);
    const error = el("p", "form-error");
    error.hidden = true;
    const submit = el("button", "button-primary", icon("plus", 14), "Add");
    submit.type = "submit";
    const cancel = button("Cancel", () => {
      dialog.close();
      resolve(null);
    });
    const form = el(
      "form",
      "dialog",
      el(
        "div",
        "dialog-head",
        kindTile(options.kind, options.name, options.details, 22),
        el("div", "", el("h2", "", "Add device"), el("p", "dialog-sub", options.subtitle)),
      ),
      el("label", "field", el("span", "field-label", "Name"), name),
      el("label", "field", el("span", "field-label", "Room"), room),
      ...options.notes,
      error,
      el("div", "dialog-actions", cancel, submit),
    );
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      submit.disabled = true;
      try {
        const saved = await options.save(name.value, room.value.trim() || null);
        dialog.close();
        resolve(saved);
      } catch (err) {
        error.textContent = api.errorMessage(err);
        error.hidden = false;
        submit.disabled = false;
      }
    });
    const dialog = modal(form, () => resolve(null));
    name.focus();
    name.select();
  });
}

export function openAddDialog(device: Device): Promise<SavedDevice | null> {
  return nameAndRoom({
    kind: device.kind,
    name: device.name,
    details: device.details,
    subtitle: subtitle(device),
    notes: [
      el("p", "hint", howChecked(device.endpoints)),
      ...(device.kind === "ready_to_add"
        ? [
            el(
              "p",
              "hint warn",
              "This device is waiting to be set up. Thingscan will remember it and watch for it, but joining it to your Wi-Fi still happens in its maker's app.",
            ),
          ]
        : []),
    ],
    save: (name, room) => api.addDevice(device, name, room),
  });
}

export function openAddCloudDialog(device: CloudDevice): Promise<SavedDevice | null> {
  return nameAndRoom({
    kind: kindOfCategory(device.category),
    name: device.name,
    details: { "cloud.category": device.category ?? "" },
    subtitle: ["SmartThings", categoryLabel(device.category), device.model].filter(Boolean).join(" · "),
    notes: [el("p", "hint", "Controlled through your SmartThings account, which also tells Thingscan whether it's online.")],
    save: (name, room) => api.cloudAdd(device.deviceId, name, room),
  });
}

/** Changes a saved device's name and room. Resolves to the saved device, or null if cancelled. */
export function openEditDialog(device: SavedDevice): Promise<SavedDevice | null> {
  return new Promise((resolve) => {
    const name = el("input", "input");
    name.value = device.name;
    const room = roomInput(device.room);
    room.placeholder = "No room";
    const error = el("p", "form-error");
    error.hidden = true;
    const submit = el("button", "button-primary", icon("check", 14), "Save");
    submit.type = "submit";
    const cancel = button("Cancel", () => {
      dialog.close();
      resolve(null);
    });
    const form = el(
      "form",
      "dialog",
      el(
        "div",
        "dialog-head",
        kindTile(device.kind, device.name, device.details, 22),
        el("div", "", el("h2", "", "Edit device"), el("p", "dialog-sub", subtitle(device))),
      ),
      el("label", "field", el("span", "field-label", "Name"), name),
      el("label", "field", el("span", "field-label", "Room"), room),
      el("p", "hint", "Pick an existing room, type a new one, or leave it empty."),
      error,
      el("div", "dialog-actions", cancel, submit),
    );
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      if (!name.value.trim()) {
        error.textContent = "Give it a name.";
        error.hidden = false;
        name.focus();
        return;
      }
      submit.disabled = true;
      try {
        const saved = await api.updateDevice(device.id, name.value, room.value.trim() || null);
        dialog.close();
        resolve(saved);
      } catch (err) {
        error.textContent = api.errorMessage(err);
        error.hidden = false;
        submit.disabled = false;
      }
    });
    const dialog = modal(form, () => resolve(null));
    name.focus();
    name.select();
  });
}

/** Renames a room for every device in it. Resolves to the moved devices, or null if cancelled. */
export function openRoomDialog(room: string, count: number): Promise<SavedDevice[] | null> {
  return new Promise((resolve) => {
    const input = el("input", "input");
    input.value = room;
    const error = el("p", "form-error");
    error.hidden = true;
    const submit = el("button", "button-primary", icon("check", 14), "Save");
    submit.type = "submit";
    const form = el(
      "form",
      "dialog",
      el(
        "div",
        "dialog-head",
        el("span", "kind-tile", icon("pin", 20)),
        el(
          "div",
          "",
          el("h2", "", "Rename room"),
          el("p", "dialog-sub", `${count} device${count === 1 ? "" : "s"} in ${room}`),
        ),
      ),
      el("label", "field", el("span", "field-label", "Room name"), input),
      el("p", "hint", "Leave it empty to take these devices out of any room. Using another room's name merges them."),
      error,
      el(
        "div",
        "dialog-actions",
        button("Cancel", () => {
          dialog.close();
          resolve(null);
        }),
        submit,
      ),
    );
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      submit.disabled = true;
      try {
        const moved = await api.renameRoom(room, input.value.trim() || null);
        dialog.close();
        resolve(moved);
      } catch (err) {
        error.textContent = api.errorMessage(err);
        error.hidden = false;
        submit.disabled = false;
      }
    });
    const dialog = modal(form, () => resolve(null));
    input.focus();
    input.select();
  });
}

const TOKENS_URL = "https://account.smartthings.com/tokens";
const REDIRECT_URI = "https://httpbin.org/get";

/** How the connect dialog ended: the account, and why its token can't renew itself, if so. */
export interface Connection {
  state: api.AccountState;
  appError: string | null;
}

/**
 * Connects SmartThings. A personal token works straight away but lasts 24 hours; with Apps
 * ticked, Thingscan uses it once to set up its own SmartThings app, and signing in through that
 * app (the second step, where `signIn` starts) gets a token that renews itself.
 */
export function openConnectDialog(reason?: string, signIn = false): Promise<Connection | null> {
  return new Promise((resolve) => {
    let step: "token" | "signIn" = signIn ? "signIn" : "token";
    let connected: Connection | null = null;
    const token = el("input", "input mono");
    token.type = "password";
    token.placeholder = "Paste the token here";
    token.autocomplete = "off";
    token.spellcheck = false;
    const clientId = el("input", "input mono");
    clientId.autocomplete = "off";
    clientId.spellcheck = false;
    const clientSecret = el("input", "input mono");
    clientSecret.type = "password";
    clientSecret.autocomplete = "off";
    const error = el("p", "form-error");
    error.hidden = !reason;
    error.textContent = reason ?? "";
    const submit = el("button", "button-primary");
    submit.type = "submit";
    const cancel = button("Cancel", () => finish(connected));
    const sub = el("p", "dialog-sub");
    const copyLabel = el("span", "", "Copy link");
    const copy = button(
      [icon("copy", 13), copyLabel],
      () => {
        void navigator.clipboard?.writeText(TOKENS_URL);
        copyLabel.textContent = "Copied";
      },
      "button-secondary button-small",
    );
    const head = el(
      "div",
      "dialog-head",
      el("span", "kind-tile", icon("cloud", 22)),
      el("div", "", el("h2", "", "Connect SmartThings"), sub),
    );
    const actions = el("div", "dialog-actions", cancel, submit);

    const tokenStep = (): Node[] => [
      el(
        "p",
        "dialog-body",
        "Samsung appliances only take commands from the account that owns them, so Thingscan controls them through your SmartThings account, the way the SmartThings app does. Your TV's remote works without it.",
      ),
      el(
        "ol",
        "steps",
        el("li", "", "Open ", el("span", "mono-inline", TOKENS_URL), " in a browser and sign in. ", copy),
        el(
          "li",
          "",
          "Generate a new token. Under Devices, tick List all devices, See all devices and Control all devices. Under Apps, tick everything as well.",
        ),
        el("li", "", "Paste the token below."),
      ),
      el("label", "field", el("span", "field-label", "Personal access token"), token),
      el(
        "p",
        "hint",
        "SmartThings makes personal tokens last 24 hours. With Apps ticked, Thingscan uses this one once, to set up its own SmartThings app; after you sign in through it, Thingscan renews its access by itself. Tokens are kept in Thingscan's data folder, readable only by you.",
      ),
      el(
        "details",
        "disclosure",
        el("summary", "", "Made an OAuth app with the SmartThings CLI?"),
        el(
          "p",
          "hint",
          "Enter its client ID and secret instead of a token. Its redirect URI must be ",
          el("span", "mono-inline", REDIRECT_URI),
          ".",
        ),
        el("label", "field", el("span", "field-label", "Client ID"), clientId),
        el("label", "field", el("span", "field-label", "Client secret"), clientSecret),
      ),
      error,
      actions,
    ];
    const signInStep = (): Node[] => [
      el(
        "p",
        "dialog-body",
        "Sign in with your Samsung account in the SmartThings window and choose Allow. From then on Thingscan renews its access every day by itself, here and, through your Thingscan account, on your iPhone.",
      ),
      error,
      actions,
    ];
    const render = () => {
      if (step === "token") {
        sub.textContent = "For air conditioners, washers and other appliances";
        submit.replaceChildren(icon("link", 14), "Connect");
      } else {
        sub.textContent = "So you never need another token";
        submit.replaceChildren(icon("logIn", 14), "Sign in to SmartThings");
        cancel.textContent = connected ? "Not now" : "Cancel";
      }
      form.replaceChildren(head, ...(step === "token" ? tokenStep() : signInStep()));
    };
    const finish = (result: Connection | null) => {
      dialog.close();
      resolve(result);
    };

    const form = el("form", "dialog dialog-wide");
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      submit.disabled = true;
      error.hidden = true;
      try {
        if (step === "token") {
          if (clientId.value.trim() || clientSecret.value.trim()) {
            await api.cloudSetApp(clientId.value, clientSecret.value);
          } else {
            const result = await api.cloudConnect(token.value);
            connected = { state: result.account, appError: result.appError };
            if (!result.account.hasApp) return finish(connected);
          }
          step = "signIn";
          render();
        } else {
          submit.replaceChildren(icon("spinner", 14), "Waiting for SmartThings…");
          finish({ state: await api.cloudSignIn(), appError: null });
        }
      } catch (err) {
        error.textContent = api.errorMessage(err);
        error.hidden = false;
        render();
      } finally {
        submit.disabled = false;
      }
    });
    render();
    const dialog = modal(form, () => resolve(connected));
    if (step === "token") token.focus();
  });
}

const SERVER_KEY = "thingscan.server";

/** Signs in to a Thingscan account, or creates one. Resolves to the account once signed in. */
export function openAccountDialog(mode: "signIn" | "signUp"): Promise<api.ThingscanAccount | null> {
  return new Promise((resolve) => {
    let creating = mode === "signUp";
    const server = el("input", "input mono");
    server.placeholder = "sync.example.com";
    server.spellcheck = false;
    server.autocapitalize = "off";
    try {
      server.value = localStorage.getItem(SERVER_KEY) ?? "";
    } catch {
      // Private storage unavailable; the field just starts empty.
    }
    const email = el("input", "input");
    email.type = "email";
    email.autocomplete = "username";
    email.placeholder = "you@example.com";
    const password = el("input", "input");
    password.type = "password";
    const confirm = el("input", "input");
    confirm.type = "password";
    confirm.autocomplete = "new-password";
    const confirmField = el("label", "field", el("span", "field-label", "Repeat password"), confirm);
    const title = el("h2", "");
    const sub = el("p", "dialog-sub");
    const error = el("p", "form-error");
    error.hidden = true;
    const submit = el("button", "button-primary");
    submit.type = "submit";
    const switchMode = button("", () => {
      creating = !creating;
      error.hidden = true;
      show();
    }, "button-link");
    const show = () => {
      title.textContent = creating ? "Create a Thingscan account" : "Sign in to Thingscan";
      sub.textContent = creating
        ? "Sync your devices with your iPhone and other Macs, from anywhere"
        : "Bring your devices to this Mac and keep them in sync";
      submit.replaceChildren(icon(creating ? "plus" : "logIn", 14), creating ? "Create account" : "Sign in");
      switchMode.textContent = creating ? "I already have an account" : "Create an account instead";
      confirmField.hidden = !creating;
      password.autocomplete = creating ? "new-password" : "current-password";
      password.placeholder = creating ? "At least 8 characters" : "";
    };
    const form = el(
      "form",
      "dialog",
      el("div", "dialog-head", el("span", "kind-tile", icon("user", 22)), el("div", "", title, sub)),
      el("label", "field", el("span", "field-label", "Server"), server),
      el("label", "field", el("span", "field-label", "Email"), email),
      el("label", "field", el("span", "field-label", "Password"), password),
      confirmField,
      el(
        "p",
        "hint",
        "Your devices and SmartThings token are encrypted with your password before they leave this Mac, so the server can't read them. That also means a forgotten password can't be reset.",
      ),
      error,
      el(
        "div",
        "dialog-actions",
        switchMode,
        button("Cancel", () => {
          dialog.close();
          resolve(null);
        }),
        submit,
      ),
    );
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const fail = (message: string, field?: HTMLInputElement) => {
        error.textContent = message;
        error.hidden = false;
        field?.focus();
      };
      if (!server.value.trim()) return fail("Enter your Thingscan server's address.", server);
      if (!email.value.trim()) return fail("Enter your email address.", email);
      if (creating && password.value !== confirm.value) return fail("The passwords don't match.", confirm);
      submit.disabled = true;
      error.hidden = true;
      const label = submit.textContent;
      submit.replaceChildren(icon("spinner", 14), creating ? "Creating…" : "Signing in…");
      try {
        const status = creating
          ? await api.accountSignUp(server.value, email.value, password.value)
          : await api.accountSignIn(server.value, email.value, password.value);
        try {
          localStorage.setItem(SERVER_KEY, server.value.trim());
        } catch {
          // Not remembered; nothing else depends on it.
        }
        dialog.close();
        resolve(status);
      } catch (err) {
        fail(api.errorMessage(err));
        show();
        submit.disabled = false;
        if (!submit.textContent) submit.textContent = label;
      }
    });
    show();
    const dialog = modal(form, () => resolve(null));
    (server.value ? email : server).focus();
  });
}

export function confirmDialog(title: string, body: string, confirmLabel: string): Promise<boolean> {
  return new Promise((resolve) => {
    const finish = (answer: boolean) => {
      dialog.close();
      resolve(answer);
    };
    const confirm = button([icon("trash", 14), confirmLabel], () => finish(true), "button-danger");
    const content = el(
      "div",
      "dialog",
      el("h2", "", title),
      el("p", "dialog-body", body),
      el(
        "div",
        "dialog-actions",
        button("Cancel", () => finish(false)),
        confirm,
      ),
    );
    const dialog = modal(content, () => resolve(false));
    confirm.focus();
  });
}
