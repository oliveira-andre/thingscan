// Pieces shared by the home, the device drawer and the add screen.
import type { Endpoint, Kind, Probe, SavedDevice } from "./api";
import { el } from "./dom";
import { ago, deviceIcon } from "./format";
import { icon } from "./icons";
import type { Check } from "./state";

export function kindTile(kind: Kind, name: string, details: Record<string, string>, size = 20): HTMLElement {
  return el("span", `kind-tile kind-${kind}`, icon(deviceIcon(kind, name, details), size));
}

export type Tone = "checking" | "reachable" | "unreachable" | "unknown" | "idle";

export function statusOf(check: Check | undefined, device: SavedDevice): { tone: Tone; text: string } {
  if (check === "checking") {
    return { tone: "checking", text: "Checking…" };
  }
  const seen = device.lastSeen ? `seen ${ago(device.lastSeen)}` : "never seen since added";
  if (!check) {
    return { tone: "idle", text: device.lastSeen ? `Not checked yet · ${seen}` : "Not checked yet" };
  }
  const ok = check.probes.filter((p) => p.status === "reachable");
  switch (check.status) {
    case "reachable": {
      // A TV in standby still shows as online in SmartThings, so don't call it reachable here.
      if (ok.length > 0 && ok.every((p) => p.via === "SmartThings")) return { tone: "reachable", text: "Online on API" };
      const fastest = ok.filter((p) => p.latencyMs !== null).sort((a, b) => a.latencyMs! - b.latencyMs!)[0];
      const extra = fastest ? `${fastest.latencyMs} ms` : ok[0]?.detail;
      return { tone: "reachable", text: extra ? `Reachable · ${extra}` : "Reachable" };
    }
    case "unreachable":
      return { tone: "unreachable", text: `Not responding · ${seen}` };
    default:
      return { tone: "unknown", text: `Can't check · ${check.probes[0]?.detail ?? "nothing to check"}` };
  }
}

const ENDPOINT_LABELS: Record<Endpoint["type"], string> = {
  ocf: "OCF",
  upnp: "UPnP",
  samsungTv: "TV API",
  mdns: "mDNS",
  ble: "BLE",
  softAp: "Wi-Fi",
  cloud: "API",
};

/** A check's channel as a tag; the SmartThings cloud check shows as "API". */
export function viaLabel(via: string): string {
  return via === "SmartThings" ? "API" : via;
}

/** One chip per way of reaching the device, coloured by the latest result. */
export function viaChips(check: Check | undefined, endpoints: Endpoint[]): HTMLElement {
  const chips = el("div", "chips");
  if (check && check !== "checking") {
    const seen = new Set<string>();
    for (const probe of check.probes) {
      if (!seen.has(probe.via)) {
        seen.add(probe.via);
        chips.append(chip(viaLabel(probe.via), probe.status, probeTitle(probe)));
      }
    }
  } else {
    for (const label of new Set(endpoints.map((e) => ENDPOINT_LABELS[e.type]))) {
      chips.append(chip(label, "idle"));
    }
  }
  return chips;
}

export function chip(label: string, tone: string, title?: string): HTMLElement {
  const node = el("span", `chip ${tone}`, label);
  if (title) {
    node.title = title;
  }
  return node;
}

export function probeTitle(p: Probe): string {
  return [p.target, p.detail, p.latencyMs !== null ? `${p.latencyMs} ms` : ""].filter(Boolean).join(" — ");
}

export function statusIcon(tone: Tone): SVGElement | HTMLElement {
  return tone === "checking" ? icon("spinner", 14) : el("span", `status-dot ${tone}`);
}
