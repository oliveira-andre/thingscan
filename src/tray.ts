// The menu bar panel: every added device with its status, and its controls one click away
// (the remote for a TV, the appliance controls for an air conditioner or washer). It shares
// devices and check results with the main window.
import "./fonts";
import markUrl from "./assets/mark.svg";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { loadAccount } from "./account";
import * as api from "./api";
import type { SavedDevice } from "./api";
import { mountControls } from "./controls";
import { button, el } from "./dom";
import { isTv } from "./drawer";
import { clock, subtitle } from "./format";
import { icon, iconButton } from "./icons";
import { keyedList } from "./keyed";
import { mountRemote } from "./remote";
import { checkDevices, loadDevices, state, subscribe, syncWithOtherWindows } from "./state";
import { toast } from "./toast";
import { kindTile, statusIcon, statusOf } from "./widgets";

/** Opening the panel re-checks devices when the last check is older than this. */
const STALE_MS = 2 * 60_000;

const root = document.querySelector<HTMLElement>("#panel")!;

/** The device whose controls are showing, kept while the panel is hidden. */
let selected: string | null = null;
let unmountDetail: (() => void) | null = null;
let detailFor: string | null = null;
/** Controls only run while the panel is on screen, so a hidden panel holds no TV connection. */
let shown = false;

const head = el("header", "panel-head");
const body = el("div", "panel-body");
const list = el("div", "panel-list");
const foot = el(
  "footer",
  "panel-foot",
  button([icon("plus", 14), "Add device"], () => void api.openMain("add"), "button-secondary button-small"),
  button("Open Thingscan", () => void api.openMain(), "button-primary button-small"),
);
root.replaceChildren(head, body, foot);

function render(): void {
  const device = selected ? state.devices.find((d) => d.id === selected) : undefined;
  if (selected && !device) {
    selected = null;
  }
  if (device) {
    renderDetailHead(device);
    if (shown && detailFor !== device.id) {
      mountDetail(device);
    }
  } else {
    closeDetail();
    renderListHead();
    renderList();
  }
}

function renderListHead(): void {
  const reachable = state.devices.filter((d) => {
    const check = state.checks.get(d.id);
    return check && check !== "checking" && check.status === "reachable";
  }).length;
  const summary =
    state.devices.length === 0
      ? "No devices yet"
      : [
          `${state.devices.length} device${state.devices.length === 1 ? "" : "s"}`,
          state.checking ? "checking…" : `${reachable} reachable`,
          state.lastCheck && !state.checking ? clock(state.lastCheck) : null,
        ]
          .filter(Boolean)
          .join(" · ");
  const refresh = iconButton(state.checking ? "spinner" : "refresh", "Check every device", () => void checkDevices());
  refresh.disabled = state.checking || state.devices.length === 0;
  head.replaceChildren(
    el(
      "div",
      "panel-brand",
      el("img", "panel-mark"),
      el("div", "", el("div", "panel-title", "Thingscan"), el("div", "panel-summary", summary)),
    ),
    refresh,
  );
  head.querySelector<HTMLImageElement>(".panel-mark")!.src = markUrl;
}

function renderList(): void {
  if (state.devices.length === 0) {
    body.replaceChildren(
      el(
        "div",
        "panel-empty",
        el("div", "empty-art", icon("radar", 32)),
        el("p", "", "Add your TV, air conditioners and other devices in Thingscan, then control them from here."),
        button([icon("plus", 14), "Add device"], () => void api.openMain("add"), "button-primary"),
      ),
    );
    return;
  }
  if (!body.contains(list)) {
    body.replaceChildren(list);
  }
  // Rooms in order, then devices without one.
  const rooms = [...new Set(state.devices.map((d) => d.room ?? ""))].sort((a, b) =>
    a === "" ? 1 : b === "" ? -1 : a.localeCompare(b),
  );
  const items: ({ type: "room"; room: string } | { type: "device"; device: SavedDevice })[] = [];
  for (const room of rooms) {
    if (rooms.length > 1 || room) items.push({ type: "room", room });
    for (const device of state.devices.filter((d) => (d.room ?? "") === room).sort((a, b) => a.name.localeCompare(b.name))) {
      items.push({ type: "device", device });
    }
  }
  keyedList(
    list,
    items,
    (item) => (item.type === "room" ? `room:${item.room}` : item.device.id),
    (item) => (item.type === "room" ? item.room : JSON.stringify([item.device, state.checks.get(item.device.id)])),
    (item) => (item.type === "room" ? el("div", "panel-room", item.room || "Other devices") : row(item.device)),
  );
}

function row(device: SavedDevice): HTMLElement {
  const check = state.checks.get(device.id);
  const status = statusOf(check, device);
  const node = el(
    "button",
    `panel-row tone-${status.tone}`,
    kindTile(device.kind, device.name, device.details, 18),
    el("div", "panel-row-main", el("div", "panel-row-name", device.name), el("div", "panel-row-status", status.text)),
    el("span", "panel-row-action", isTv(device) ? "Remote" : "Control", icon("chevronRight", 14)),
  );
  node.type = "button";
  node.title = subtitle(device);
  node.addEventListener("click", () => {
    selected = device.id;
    render();
  });
  return node;
}

function renderDetailHead(device: SavedDevice): void {
  const status = statusOf(state.checks.get(device.id), device);
  head.replaceChildren(
    iconButton("back", "All devices", () => {
      selected = null;
      render();
    }),
    el(
      "div",
      "panel-brand",
      kindTile(device.kind, device.name, device.details, 18),
      el(
        "div",
        "",
        el("div", "panel-title", device.name),
        el("div", `panel-summary tone-${status.tone}`, statusIcon(status.tone), status.text),
      ),
    ),
    iconButton("external", "Open in Thingscan", () => void api.openMain()),
  );
}

function mountDetail(device: SavedDevice): void {
  closeDetail();
  const host = el("div", "control-host");
  body.replaceChildren(host);
  body.scrollTop = 0;
  unmountDetail = isTv(device) ? mountRemote(host, device) : mountControls(host, device);
  detailFor = device.id;
}

function closeDetail(): void {
  unmountDetail?.();
  unmountDetail = null;
  detailFor = null;
}

subscribe(render);

// Hidden panels let go of TV connections; showing it again reconnects.
window.addEventListener("blur", () => {
  shown = false;
  closeDetail();
});
void listen("panel-shown", async () => {
  shown = true;
  try {
    await loadDevices();
  } catch (error) {
    toast("error", "Couldn't load your devices", api.errorMessage(error));
  }
  render();
  if (!state.checking && (!state.lastCheck || Date.now() - state.lastCheck > STALE_MS)) {
    void checkDevices();
  }
});
// Escape steps back from a device, then closes the panel.
window.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || document.querySelector(".backdrop")) return;
  if (selected) {
    selected = null;
    render();
  } else {
    void getCurrentWindow().hide();
  }
});

async function boot(): Promise<void> {
  render();
  try {
    await loadDevices();
  } catch (error) {
    toast("error", "Couldn't load your devices", api.errorMessage(error));
  }
  await Promise.all([syncWithOtherWindows(), loadAccount()]);
  render();
}

void boot();
