// "Add device": scans while it is open, lists what it finds as it finds it, and adds the
// device the user picks. Leaving the screen stops the scan.
import * as api from "./api";
import { account } from "./account";
import type { CloudDevice, Device, ScanChannel } from "./api";
import { openAddCloudDialog, openAddDialog } from "./dialog";
import { button, el } from "./dom";
import { CHANNELS, categoryLabel, kindOfCategory, sourceLabel, subtitle } from "./format";
import { icon, iconButton } from "./icons";
import { keyedList } from "./keyed";
import { go } from "./router";
import { checkDevices, state, subscribe, upsertDevice } from "./state";
import { toast } from "./toast";
import { chip, kindTile } from "./widgets";

/** Matches SCAN_TIME in src-tauri/src/commands.rs; drives the progress bar. */
const SCAN_MS = 10_000;

interface ChannelState {
  phase: "scanning" | "done" | "error";
  found: number;
  error?: string;
}

export function mountAdd(root: HTMLElement): () => void {
  let devices: Device[] = [];
  let channels = new Map<ScanChannel, ChannelState>();
  let finished = false;
  let disposed = false;

  const title = el("div", "page-title", el("h1", "", "Add device"), el("p", "page-summary"));
  const again = button([icon("refresh"), "Scan again"], () => start());
  const head = el(
    "div",
    "page-head",
    el("div", "head-left", iconButton("back", "Back to home", () => go("home")), title),
    el("div", "page-actions", again),
  );
  const bar = el("div", "progress-fill");
  const chips = el("div", "channel-chips");
  const panel = el("div", "scan-panel", el("div", "progress", bar), chips);
  const results = el("div", "results");
  root.replaceChildren(el("div", "page", head, panel, results));

  // Wi-Fi can take 15s+ on macOS; the scan counts as running only while the others are.
  const busy = () => !finished && [...channels].some(([c, s]) => c !== "wifi" && s.phase === "scanning");

  const render = () => {
    const running = busy();
    title.querySelector(".page-summary")!.textContent = running
      ? "Looking on your network and over Bluetooth…"
      : `Scan finished · ${devices.length} found`;
    again.disabled = running;
    panel.classList.toggle("running", running);
    renderChannels(chips, channels);
    renderResults(results, devices, running);
  };

  const start = () => {
    devices = [];
    finished = false;
    channels = new Map(CHANNELS.map(({ channel }) => [channel, { phase: "scanning", found: 0 }]));
    // Restart the progress animation.
    bar.classList.remove("animate");
    void bar.offsetWidth;
    bar.style.animationDuration = `${SCAN_MS}ms`;
    bar.classList.add("animate");
    render();
    api
      .startScan((event) => {
        if (disposed) return;
        switch (event.event) {
          case "started":
            channels.set(event.channel, { phase: "scanning", found: 0 });
            break;
          case "devices":
            devices = event.devices;
            break;
          case "done":
            channels.set(event.channel, {
              phase: event.error ? "error" : "done",
              found: event.found,
              error: event.error ?? undefined,
            });
            break;
          case "finished":
            devices = event.devices;
            finished = true;
            break;
        }
        render();
      })
      .catch((error) => {
        finished = true;
        toast("error", "Couldn't start scanning", api.errorMessage(error));
        render();
      });
  };

  const unsubscribe = subscribe(render);
  start();
  return () => {
    disposed = true;
    unsubscribe();
    if (!finished) {
      void api.cancelScan();
    }
  };
}

function renderChannels(container: HTMLElement, channels: Map<ScanChannel, ChannelState>): void {
  keyedList(
    container,
    CHANNELS,
    (c) => c.channel,
    (c) => JSON.stringify(channels.get(c.channel)),
    ({ channel, label, title }) => {
      const s = channels.get(channel) ?? { phase: "scanning", found: 0 };
      const mark =
        s.phase === "scanning" ? icon("spinner", 13) : s.phase === "error" ? icon("alert", 13) : icon("check", 13);
      const count = s.phase === "done" ? el("span", "channel-count", String(s.found)) : null;
      const node = el("span", `channel ${s.phase}`, mark, label, count);
      node.title = s.error ? `${title}\n${s.error}` : title;
      return node;
    },
  );
}

interface Section {
  id: string;
  title: string;
  hint: string;
  devices?: Device[];
  cloud?: CloudDevice[];
}

function renderResults(container: HTMLElement, devices: Device[], running: boolean): void {
  const cloud = cloudOnly(devices);
  if (devices.length === 0 && cloud.length === 0) {
    container.replaceChildren(
      running
        ? el("div", "empty scanning", el("div", "empty-art pulse", icon("radar", 40)), el("h2", "", "Looking for devices…"))
        : el(
            "div",
            "empty",
            el("div", "empty-art", icon("radar", 40)),
            el("h2", "", "Nothing found nearby"),
            el(
              "p",
              "",
              "Make sure the device is on and on the same network, or put it in pairing mode so it advertises over Bluetooth. Then scan again.",
            ),
          ),
    );
    return;
  }
  if (container.querySelector(".empty")) {
    container.replaceChildren();
  }
  const sections: Section[] = [
    {
      id: "ready",
      title: "Ready to add",
      hint: "Waiting to be set up",
      devices: devices.filter((d) => d.kind === "ready_to_add"),
    },
    {
      id: "nearby",
      title: "Found nearby",
      hint: "On your network or in Bluetooth range",
      devices: devices.filter((d) => d.kind !== "ready_to_add"),
    },
    { id: "cloud", title: "In your SmartThings account", hint: "Not seen nearby, controlled through SmartThings", cloud },
  ].filter((s) => (s.devices ?? s.cloud ?? []).length > 0);

  const nodes = keyedList(
    container,
    sections,
    (s) => s.id,
    (s) => s.id,
    (s) =>
      el(
        "section",
        "result-section",
        el("h2", "room-title", s.title, el("span", "section-hint", s.hint)),
        el("div", "rows"),
      ),
  );
  nodes.forEach((node, i) => {
    const rows = node.querySelector<HTMLElement>(".rows")!;
    const section = sections[i];
    if (section.cloud) {
      keyedList(rows, section.cloud, (d) => d.deviceId, (d) => JSON.stringify(d), cloudRow);
      return;
    }
    keyedList(
      rows,
      section.devices ?? [],
      (d) => d.key,
      (d) => JSON.stringify([d.name, d.kind, d.address, d.sources, d.model, d.manufacturer, savedId(d)]),
      row,
    );
  });
}

/** SmartThings devices that aren't in the home yet and weren't found nearby by this scan. */
function cloudOnly(scanned: Device[]): CloudDevice[] {
  if (!account.connected || account.error) return [];
  const linked = new Set(state.devices.map((d) => d.cloudId));
  return account.devices.filter(
    (c) => !linked.has(c.deviceId) && !(c.di && scanned.some((d) => d.ids.includes(`ocf:${c.di}`))),
  );
}

function cloudRow(device: CloudDevice): HTMLElement {
  const kind = kindOfCategory(device.category);
  return el(
    "div",
    "result-row",
    kindTile(kind, device.name, { "cloud.category": device.category ?? "" }),
    el(
      "div",
      "row-main",
      el("div", "row-name", device.name),
      el("div", "row-sub", [categoryLabel(device.category), device.model].filter(Boolean).join(" · ")),
    ),
    el("div", "chips", chip("SmartThings", "idle")),
    button([icon("plus", 14), "Add"], () => void addCloud(device), "button-primary button-small"),
  );
}

async function addCloud(device: CloudDevice): Promise<void> {
  const saved = await openAddCloudDialog(device);
  if (!saved) return;
  upsertDevice(saved);
  toast("success", `Added ${saved.name}`, saved.room ? `In ${saved.room}` : undefined);
  go("home");
  void checkDevices([saved.id]);
}

/** The saved device this scan result is, if it was added before. */
function savedId(device: Device): string | null {
  return state.devices.find((saved) => saved.ids.some((id) => device.ids.includes(id)))?.id ?? null;
}

function row(device: Device): HTMLElement {
  const added = savedId(device) !== null;
  const action = added
    ? el("span", "badge-added", icon("check", 13), "Added")
    : button([icon("plus", 14), "Add"], () => void add(device), "button-primary button-small");
  return el(
    "div",
    "result-row",
    kindTile(device.kind, device.name, device.details),
    el(
      "div",
      "row-main",
      el("div", "row-name", device.name),
      el(
        "div",
        "row-sub",
        subtitle(device),
        el("span", "row-address", device.address),
      ),
    ),
    el("div", "chips", ...device.sources.map((s) => chip(sourceLabel(s), "idle"))),
    action,
  );
}

async function add(device: Device): Promise<void> {
  const saved = await openAddDialog(device);
  if (!saved) return;
  upsertDevice(saved);
  toast("success", `Added ${saved.name}`, saved.room ? `In ${saved.room}` : undefined);
  go("home");
  void checkDevices([saved.id]);
}
