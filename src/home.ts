// The home: every added device grouped by room, with whether it answered the last check.
import { account } from "./account";
import type { SavedDevice } from "./api";
import { button, el } from "./dom";
import { openRoomDialog } from "./dialog";
import { editDevice, openDrawer } from "./drawer";
import { clock, subtitle } from "./format";
import { icon, iconButton } from "./icons";
import { keyedList } from "./keyed";
import { go } from "./router";
import { checkDevices, state, subscribe, upsertDevice } from "./state";
import { toast } from "./toast";
import { kindTile, statusIcon, statusOf, viaChips } from "./widgets";

const NO_ROOM = "";

export function mountHome(root: HTMLElement): () => void {
  const head = el("div", "page-head");
  const content = el("div", "home-content");
  const page = el("div", "page", head, content);
  root.replaceChildren(page);

  let headSignature = "";
  const render = () => {
    const signature = JSON.stringify([
      state.devices.length,
      reachableCount(),
      state.checking,
      state.lastCheck,
      account.connected,
      account.error,
      account.devices.length,
    ]);
    if (signature !== headSignature) {
      headSignature = signature;
      head.replaceChildren(...renderHead());
    }
    if (state.devices.length === 0) {
      content.replaceChildren(emptyState());
      return;
    }
    if (content.querySelector(".empty")) {
      content.replaceChildren();
    }
    renderRooms(content);
  };
  render();
  return subscribe(render);
}

function reachableCount(): number {
  return state.devices.filter((d) => {
    const check = state.checks.get(d.id);
    return check && check !== "checking" && check.status === "reachable";
  }).length;
}

function renderHead(): Node[] {
  const count = state.devices.length;
  const parts =
    count === 0
      ? ["No devices yet"]
      : [
          `${count} device${count === 1 ? "" : "s"}`,
          state.checking ? "checking…" : `${reachableCount()} reachable`,
          state.lastCheck && !state.checking ? `checked ${clock(state.lastCheck)}` : null,
        ].filter((p): p is string => !!p);

  const refresh = button(
    [icon(state.checking ? "spinner" : "refresh"), state.checking ? "Checking" : "Check again"],
    () => void checkDevices(),
  );
  refresh.disabled = state.checking || count === 0;
  refresh.title = "Try to reach every device again";
  const add = button([icon("plus"), "Add device"], () => go("add"), "button-primary pill");
  return [
    el("div", "page-title", el("h1", "", "Home"), el("p", "page-summary", parts.join(" · "))),
    el("div", "page-actions", refresh, add),
  ];
}

function renderRooms(content: HTMLElement): void {
  const groups = new Map<string, SavedDevice[]>();
  for (const device of state.devices) {
    const room = device.room ?? NO_ROOM;
    groups.set(room, [...(groups.get(room) ?? []), device]);
  }
  const named = [...groups.keys()].filter((r) => r !== NO_ROOM).sort((a, b) => a.localeCompare(b));
  const order = groups.has(NO_ROOM) ? [...named, NO_ROOM] : named;
  const titled = named.length > 0;

  const sections = keyedList(
    content,
    order,
    (room) => room,
    (room) => `${room}|${titled}|${groups.get(room)!.length}`,
    (room) =>
      el(
        "section",
        "room",
        titled
          ? el(
              "h2",
              "room-title",
              icon(room === NO_ROOM ? "home" : "pin", 13),
              room || "Other devices",
              room === NO_ROOM
                ? null
                : iconButton("pencil", `Rename ${room}`, () => void renameRoom(room, groups.get(room)!.length), "icon-button room-edit"),
            )
          : null,
        el("div", "grid"),
      ),
  );
  sections.forEach((section, i) => {
    const devices = groups.get(order[i])!.sort((a, b) => a.name.localeCompare(b.name));
    keyedList(
      section.querySelector<HTMLElement>(".grid")!,
      devices,
      (d) => d.id,
      (d) => JSON.stringify([d, state.checks.get(d.id)]),
      card,
    );
  });
}

function card(device: SavedDevice): HTMLElement {
  const check = state.checks.get(device.id);
  const status = statusOf(check, device);
  const edit = iconButton(
    "pencil",
    "Edit name and room",
    (event) => {
      event.stopPropagation();
      void editDevice(device.id);
    },
    "icon-button card-edit",
  );
  // A div, not a button, so the edit button can sit inside it.
  const node = el(
    "div",
    `device-card tone-${status.tone}`,
    el(
      "div",
      "card-top",
      kindTile(device.kind, device.name, device.details),
      el("div", "card-corner", statusIcon(status.tone), edit),
    ),
    el("div", "card-name", device.name),
    el("div", "card-sub", subtitle(device)),
    el("div", "card-status", status.text),
    viaChips(check, device.endpoints),
  );
  node.tabIndex = 0;
  node.setAttribute("role", "button");
  node.setAttribute("aria-label", `${device.name}, open`);
  node.addEventListener("click", () => openDrawer(device.id));
  node.addEventListener("keydown", (event) => {
    if ((event.key === "Enter" || event.key === " ") && event.target === node) {
      event.preventDefault();
      openDrawer(device.id);
    }
  });
  return node;
}

async function renameRoom(room: string, count: number): Promise<void> {
  const moved = await openRoomDialog(room, count);
  if (!moved) return;
  for (const device of moved) upsertDevice(device);
  const to = moved[0]?.room;
  toast("success", to ? `Renamed to ${to}` : `Took ${count} device${count === 1 ? "" : "s"} out of ${room}`);
}

function emptyState(): HTMLElement {
  return el(
    "div",
    "empty",
    el("div", "empty-art", icon("radar", 40)),
    el("h2", "", "No devices yet"),
    el(
      "p",
      "",
      "Add your TV, appliances, hubs and Matter devices. Thingscan finds them on your network and over Bluetooth, then checks they still answer each time you open it.",
    ),
    button([icon("plus"), "Add device"], () => go("add"), "button-primary pill"),
  );
}
