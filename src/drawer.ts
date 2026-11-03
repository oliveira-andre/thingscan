// The panel that opens from a device card. It opens on the TV remote for a TV, or on the
// appliance controls for anything else; Details shows how the device was reached, checks it
// again, or removes it. The header's edit button changes its name and room.
import * as api from "./api";
import type { SavedDevice } from "./api";
import { confirmDialog, openEditDialog } from "./dialog";
import { button, el } from "./dom";
import { ago, date, kindLabel } from "./format";
import { icon, iconButton } from "./icons";
import { mountControls } from "./controls";
import { mountRemote } from "./remote";
import { checkDevices, dropDevice, state, subscribe, upsertDevice } from "./state";
import { toast } from "./toast";
import { kindTile, statusIcon, statusOf, viaLabel } from "./widgets";

let closeOpen: (() => void) | null = null;

export function isTv(device: SavedDevice): boolean {
  return device.kind === "tv" || device.endpoints.some((e) => e.type === "samsungTv");
}

type Tab = "control" | "details";

export function openDrawer(id: string): void {
  closeOpen?.();
  const device = state.devices.find((d) => d.id === id);
  if (!device) return;

  const title = el("h2", "drawer-title", device.name);
  const place = el("p", "drawer-room");
  const edit = iconButton("pencil", "Edit name and room", () => void editDevice(id));

  const body = el("div", "drawer-body");
  const details = el("div", "drawer-details", body);
  const tabBody = el("div", "tab-body");
  const tabButtons = new Map<Tab, HTMLButtonElement>();
  const tabs = el("div", "tabs");
  for (const [tab, label] of [
    ["control", isTv(device) ? "Remote" : "Controls"],
    ["details", "Details"],
  ] as [Tab, string][]) {
    const b = button(label, () => showTab(tab), "tab");
    tabButtons.set(tab, b);
    tabs.append(b);
  }
  let unmountTab: (() => void) | null = null;
  const showTab = (tab: Tab) => {
    unmountTab?.();
    unmountTab = null;
    for (const [t, b] of tabButtons) b.classList.toggle("selected", t === tab);
    if (tab === "control") {
      const host = el("div", "control-host");
      tabBody.replaceChildren(host);
      const current = state.devices.find((d) => d.id === id) ?? device;
      unmountTab = isTv(current) ? mountRemote(host, current) : mountControls(host, current);
    } else {
      tabBody.replaceChildren(details);
    }
  };
  const panel = el(
    "aside",
    "drawer",
    el(
      "div",
      "drawer-head",
      kindTile(device.kind, device.name, device.details, 22),
      el("div", "drawer-heading", title, place),
      edit,
      iconButton("close", "Close", () => close()),
    ),
    tabs,
    tabBody,
  );
  const backdrop = el("div", "drawer-backdrop");
  backdrop.addEventListener("mousedown", () => close());
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape" && !document.querySelector(".backdrop")) close();
  };

  let signature = "";
  const render = () => {
    const current = state.devices.find((d) => d.id === id);
    if (!current) {
      close();
      return;
    }
    title.textContent = current.name;
    place.replaceChildren(icon("pin", 12), current.room ?? "No room");
    const next = JSON.stringify([current, state.checks.get(id)]);
    if (next !== signature) {
      signature = next;
      body.replaceChildren(...renderBody(current));
    }
  };
  const unsubscribe = subscribe(render);
  const close = () => {
    unmountTab?.();
    unsubscribe();
    document.removeEventListener("keydown", onKey);
    panel.classList.add("leaving");
    backdrop.classList.add("leaving");
    window.setTimeout(() => {
      panel.remove();
      backdrop.remove();
    }, 160);
    closeOpen = null;
  };
  closeOpen = close;
  document.addEventListener("keydown", onKey);
  document.querySelector("#overlays")!.append(backdrop, panel);
  render();
  showTab("control");
}

function renderBody(device: SavedDevice): Node[] {
  const check = state.checks.get(device.id);
  const status = statusOf(check, device);
  const recheck = button(
    [icon(check === "checking" ? "spinner" : "refresh", 14), "Check now"],
    () => void checkDevices([device.id]),
    "button-secondary button-small",
  );
  recheck.disabled = check === "checking";

  const probes = el("div", "probes");
  if (check && check !== "checking") {
    for (const probe of check.probes) {
      probes.append(
        el(
          "div",
          `probe ${probe.status}`,
          el("span", `chip ${probe.status}`, viaLabel(probe.via)),
          el("span", "probe-target", probe.target),
          el("span", "probe-detail", probe.detail),
          el("span", "probe-latency", probe.latencyMs !== null ? `${probe.latencyMs} ms` : ""),
        ),
      );
    }
  }

  const facts: [string, string | null][] = [
    ["Room", device.room ?? "None"],
    ["Kind", kindLabel(device.kind)],
    ["Maker", device.manufacturer],
    ["Model", device.model],
    ["Address", device.lastAddress],
    ["Last seen", device.lastSeen ? ago(device.lastSeen) : "not since it was added"],
    ["Added", date(device.addedAt)],
  ];
  const details = Object.entries(device.details);

  const remove = button(
    [icon("trash", 14), "Remove from home"],
    async () => {
      const yes = await confirmDialog(
        `Remove ${device.name}?`,
        "Thingscan stops checking it. You can add it again from a scan.",
        "Remove",
      );
      if (!yes) return;
      try {
        await api.removeDevice(device.id);
        dropDevice(device.id);
        toast("success", `Removed ${device.name}`);
      } catch (error) {
        toast("error", "Couldn't remove", api.errorMessage(error));
      }
    },
    "button-danger-quiet",
  );

  return [
    el(
      "section",
      `status-block tone-${status.tone}`,
      el("div", "status-line", statusIcon(status.tone), el("span", "", status.text), recheck),
      probes,
    ),
    el("dl", "facts", ...facts.filter(([, v]) => v).flatMap(([k, v]) => [el("dt", "", k), el("dd", "", v!)])),
    el(
      "details",
      "disclosure",
      el("summary", "", "Identity"),
      el("ul", "mono-list", ...device.ids.map((id) => el("li", "", id))),
    ),
    details.length
      ? el(
          "details",
          "disclosure",
          el("summary", "", "What the scan saw"),
          el("dl", "facts mono", ...details.flatMap(([k, v]) => [el("dt", "", k), el("dd", "", v)])),
        )
      : null,
    el("div", "drawer-footer", remove),
  ].filter((n): n is HTMLElement => n !== null);
}

/** Opens the name and room dialog for a saved device; shared by the cards and the panel. */
export async function editDevice(id: string): Promise<void> {
  const device = state.devices.find((d) => d.id === id);
  if (!device) return;
  const saved = await openEditDialog(device);
  if (saved) {
    upsertDevice(saved);
    toast("success", "Saved", saved.room ? `${saved.name} · ${saved.room}` : saved.name);
  }
}
