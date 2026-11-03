// Controls for appliances, through the user's SmartThings account. The controls come from
// the capabilities the device reports, so an air conditioner gets temperature, mode, fan and
// swing; a washer or dryer gets its cycle state, start/pause/stop and options.
import { account, connectAccount, noteCloudError, renewAccess } from "./account";
import * as api from "./api";
import type { CloudStatus, SavedDevice } from "./api";
import { button, el } from "./dom";
import { categoryLabel, valueLabel } from "./format";
import { icon, type IconName } from "./icons";
import { state, subscribe, upsertDevice } from "./state";
import { toast } from "./toast";

type Main = Record<string, Record<string, { value: unknown; unit?: string }>>;

const POLL_MS = 10_000;
/** Polling waits this long after the user changes something, so it doesn't undo their tap. */
const QUIET_MS = 5_000;
const OPERATING_STATES = ["washerOperatingState", "dryerOperatingState", "dishwasherOperatingState"];
const MODE_ICONS: Record<string, IconName> = {
  cool: "snowflake",
  heat: "sun",
  dry: "droplets",
  wind: "wind",
  fan: "wind",
  auto: "sparkles",
};
const LIGHTING = "samsungce.airConditionerLighting";
/** Shown as their own controls, so left out of "Everything SmartThings reports". */
const HANDLED = new Set([
  "switch",
  "temperatureMeasurement",
  "relativeHumidityMeasurement",
  "thermostatCoolingSetpoint",
  "airConditionerMode",
  "airConditionerFanMode",
  "fanOscillationMode",
  "custom.airConditionerOptionalMode",
  "custom.thermostatSetpointControl",
  "remoteControlStatus",
  "powerConsumptionReport",
  "custom.washerSpinLevel",
  "custom.washerWaterTemperature",
  "custom.washerRinseCycles",
  "custom.washerSoilLevel",
  "samsungce.washerOperatingState",
  "custom.disabledCapabilities",
  LIGHTING,
  "ocf",
  "execute",
  "refresh",
  ...OPERATING_STATES,
]);

export function mountControls(container: HTMLElement, device: SavedDevice): () => void {
  let status: CloudStatus | null = null;
  let loading = false;
  let failure: string | null = null;
  let timer: number | undefined;
  let disposed = false;
  let current = device;
  let touched = 0;

  const load = async (poll = false) => {
    if (!current.cloudId || !account.connected || account.error) return;
    if (poll && Date.now() - touched < QUIET_MS) return;
    loading = !status;
    render();
    try {
      status = await api.cloudDeviceStatus(current.id);
      failure = null;
    } catch (error) {
      noteCloudError(error);
      failure = api.errorMessage(error);
    } finally {
      loading = false;
      if (!disposed) render();
    }
  };

  /** Sends a command; resolves to whether SmartThings took it. */
  const run = async (capability: string, command: string, args: unknown[] = []): Promise<boolean> => {
    touched = Date.now();
    let ok = true;
    try {
      status = await api.cloudCommand(current.id, capability, command, args);
    } catch (error) {
      noteCloudError(error);
      toast("error", "SmartThings didn't do that", api.errorMessage(error));
      ok = false;
    }
    if (!disposed) render();
    return ok;
  };

  const render = () => {
    if (disposed) return;
    container.replaceChildren(...view());
  };

  function view(): Node[] {
    if (!account.connected) {
      return [
        callout(
          "cloud",
          `Control ${current.name} through SmartThings`,
          "Samsung appliances only take commands from the account that owns them, so Thingscan sends them through your SmartThings account, like the SmartThings app does.",
          button([icon("link", 14), "Connect SmartThings"], () => void connectAccount(), "button-primary"),
        ),
      ];
    }
    if (account.error) {
      return [
        callout(
          "key",
          account.renews ? "SmartThings needs you to sign in again" : "SmartThings needs a new token",
          account.error,
          account.renews || account.hasApp
            ? button([icon("logIn", 14), "Sign in to SmartThings"], () => void renewAccess(), "button-primary")
            : button([icon("key", 14), "Paste a new token"], () => void connectAccount(), "button-primary"),
        ),
      ];
    }
    if (!current.cloudId) {
      return [linkPicker()];
    }
    if (loading) {
      return [el("div", "controls-loading", icon("spinner", 16), "Loading from SmartThings…")];
    }
    if (failure || !status) {
      return [
        callout(
          "alert",
          "Couldn't load this device",
          failure ?? "No status",
          button([icon("refresh", 14), "Try again"], () => void load(), "button-secondary"),
        ),
      ];
    }
    const main: Main = status.components?.main ?? {};
    const disabled = new Set((value(main, "custom.disabledCapabilities", "disabledCapabilities") as string[]) ?? []);
    const has = (cap: string) => cap in main && !disabled.has(cap);
    return [
      hero(main, has),
      climate(main, has),
      laundry(main, has),
      energy(main, has),
      everything(main, disabled),
    ].filter((n): n is HTMLElement => n !== null);
  }

  function linkPicker(): HTMLElement {
    const linkedIds = new Set(state.devices.map((d) => d.cloudId).filter((id): id is string => !!id));
    const candidates = account.devices.filter((d) => !linkedIds.has(d.deviceId));
    const select = el("select", "input");
    for (const d of candidates) {
      const option = el("option", "", `${d.name} · ${categoryLabel(d.category)}`);
      option.value = d.deviceId;
      select.append(option);
    }
    const link = button(
      [icon("link", 14), "Link"],
      async () => {
        try {
          const saved = await api.cloudLink(current.id, select.value);
          upsertDevice(saved);
          current = saved;
          await load();
        } catch (error) {
          toast("error", "Couldn't link", api.errorMessage(error));
        }
      },
      "button-primary",
    );
    link.disabled = candidates.length === 0;
    return callout(
      "link",
      "Which SmartThings device is this?",
      candidates.length
        ? "Thingscan couldn't match it to your SmartThings account by itself. Pick it once and it's remembered."
        : "Your SmartThings account has no devices.",
      el("div", "link-row", select, link),
    );
  }

  function hero(main: Main, has: (cap: string) => boolean): HTMLElement {
    const power = has("switch") ? value(main, "switch", "switch") : null;
    const toggle = has("switch")
      ? button(
          [icon("power", 16), power === "on" ? "On" : "Off"],
          () => void run("switch", power === "on" ? "off" : "on"),
          `power-toggle ${power === "on" ? "on" : ""}`,
        )
      : null;
    const temperature = has("temperatureMeasurement") ? value(main, "temperatureMeasurement", "temperature") : null;
    const unit = unitOf(main, "temperatureMeasurement", "temperature");
    const humidity = has("relativeHumidityMeasurement") ? value(main, "relativeHumidityMeasurement", "humidity") : null;
    const readings = el(
      "div",
      "readings",
      temperature !== null && temperature !== undefined
        ? el("div", "reading", el("span", "reading-value", `${temperature}°`), el("span", "reading-label", `Room temperature (°${unit})`))
        : null,
      humidity !== null && humidity !== undefined
        ? el("div", "reading", el("span", "reading-value small", `${humidity}%`), el("span", "reading-label", "Humidity"))
        : null,
      operatingSummary(main, has),
    );
    return el("section", "hero", readings, toggle);
  }

  function operatingSummary(main: Main, has: (cap: string) => boolean): HTMLElement | null {
    const cap = OPERATING_STATES.find(has);
    if (!cap) return null;
    const machine = String(value(main, cap, "machineState") ?? "");
    const job = value(main, cap, cap.replace("OperatingState", "JobState")) ?? value(main, "samsungce.washerOperatingState", "washerJobState");
    const remaining = minutesLeft(main, cap, machine);
    const progress = value(main, "samsungce.washerOperatingState", "progress");
    const state = machine === "run" ? "Running" : machine === "pause" ? "Paused" : "Ready";
    const parts = [job && job !== "none" ? valueLabel(job) : null, remaining !== null ? `${remaining} min left` : null].filter(Boolean);
    return el(
      "div",
      "reading",
      el("span", "reading-value small", state),
      el("span", "reading-label", parts.join(" · ") || "Not running"),
      machine === "run" && typeof progress === "number" ? progressBar(progress) : null,
    );
  }

  function climate(main: Main, has: (cap: string) => boolean): HTMLElement | null {
    const rows: HTMLElement[] = [];
    if (has("thermostatCoolingSetpoint")) {
      rows.push(setpoint(main));
    }
    const modes: [string, string, string, string, string][] = [
      ["airConditionerMode", "airConditionerMode", "supportedAcModes", "setAirConditionerMode", "Mode"],
      ["airConditionerFanMode", "fanMode", "supportedAcFanModes", "setFanMode", "Fan speed"],
      ["fanOscillationMode", "fanOscillationMode", "supportedFanOscillationModes", "setFanOscillationMode", "Swing"],
      ["custom.airConditionerOptionalMode", "acOptionalMode", "supportedAcOptionalMode", "setAcOptionalMode", "Special mode"],
    ];
    for (const [cap, attr, supportedAttr, command, label] of modes) {
      if (!has(cap)) continue;
      // Some models narrow the list for the current mode in "available…".
      const available = value(main, cap, supportedAttr.replace("supported", "available"));
      const options = (Array.isArray(available) && available.length ? available : value(main, cap, supportedAttr)) as unknown[] | null;
      if (!Array.isArray(options) || options.length === 0) continue;
      rows.push(choice(label, options.map(String), String(value(main, cap, attr) ?? ""), async (next) => void (await run(cap, command, [next])), cap === "airConditionerMode"));
    }
    const light = displayLight(main, has);
    if (light) rows.push(light);
    return rows.length ? el("section", "control-group", ...rows) : null;
  }

  /** The A/C's display light. Many Samsung models report it through samsungce.airConditionerLighting
   *  but list that capability as disabled; they take it through `execute` on the OCF resource
   *  /mode/vs/0 instead, where "Light_On" turns the light off and "Light_Off" turns it on. */
  function displayLight(main: Main, has: (cap: string) => boolean): HTMLElement | null {
    if (!(LIGHTING in main) || !(has(LIGHTING) || has("execute"))) return null;
    const direct = has(LIGHTING);
    const supported = value(main, LIGHTING, "supportedLightingLevels");
    const levels = direct && Array.isArray(supported) && supported.length ? supported.map(String) : ["on", "off"];
    return choice("Display light", levels, String(value(main, LIGHTING, "lighting") ?? ""), async (next) => {
      const ok = direct
        ? await run(LIGHTING, "setLightingLevel", [next])
        : await run("execute", "execute", ["mode/vs/0", { "x.com.samsung.da.options": [next === "off" ? "Light_On" : "Light_Off"] }]);
      if (!ok) return;
      // The A/C reports the change a moment later; show it now and read it again then.
      const reported = status?.components?.main?.[LIGHTING]?.lighting;
      if (reported) reported.value = next;
      render();
      window.setTimeout(() => void load(), 3000);
    });
  }

  function setpoint(main: Main): HTMLElement {
    const unit = unitOf(main, "thermostatCoolingSetpoint", "coolingSetpoint");
    const range = value(main, "thermostatCoolingSetpoint", "coolingSetpointRange") as { minimum?: number; maximum?: number; step?: number } | null;
    const min = range?.minimum ?? (value(main, "custom.thermostatSetpointControl", "minimumSetpoint") as number | null) ?? (unit === "F" ? 60 : 16);
    const max = range?.maximum ?? (value(main, "custom.thermostatSetpointControl", "maximumSetpoint") as number | null) ?? (unit === "F" ? 86 : 30);
    const step = range?.step ?? 1;
    let target = Number(value(main, "thermostatCoolingSetpoint", "coolingSetpoint") ?? min);
    let pending: number | undefined;
    const shown = el("span", "setpoint-value", `${target}°`);
    // Several taps in a row send one command.
    const change = (delta: number) => {
      touched = Date.now();
      target = Math.min(max, Math.max(min, Math.round((target + delta) / step) * step));
      shown.textContent = `${target}°`;
      shown.classList.add("pending");
      window.clearTimeout(pending);
      pending = window.setTimeout(() => void run("thermostatCoolingSetpoint", "setCoolingSetpoint", [target]), 700);
    };
    const minus = button(icon("minus", 18), () => change(-step), "step-button");
    const plus = button(icon("plus", 18), () => change(step), "step-button");
    minus.title = "Cooler";
    plus.title = "Warmer";
    return el(
      "div",
      "control",
      el("span", "control-label", "Target temperature"),
      el("div", "setpoint", minus, el("div", "setpoint-display", shown, el("span", "setpoint-unit", `${unit} · ${min}–${max}`)), plus),
    );
  }

  function laundry(main: Main, has: (cap: string) => boolean): HTMLElement | null {
    const cap = OPERATING_STATES.find(has);
    if (!cap) return null;
    const machine = String(value(main, cap, "machineState") ?? "");
    const supported = (value(main, cap, "supportedMachineStates") as string[] | null) ?? ["run", "pause", "stop"];
    const remote = value(main, "remoteControlStatus", "remoteControlEnabled");
    const locked = has("remoteControlStatus") && (remote === "false" || remote === false);
    const icons: Record<string, IconName> = { run: "play", pause: "pause", stop: "power" };
    const order = ["run", "pause", "stop"].filter((s) => supported.includes(s));
    const actions = el(
      "div",
      "machine-actions",
      ...order.map((state) => {
        const b = button(
          [icon(icons[state] ?? "play", 15), valueLabel(state)],
          () => void run(cap, "setMachineState", [state]),
          state === "run" ? "button-primary" : "button-secondary",
        );
        b.disabled = locked || state === machine || (state === "pause" && machine !== "run");
        return b;
      }),
    );
    const rows: HTMLElement[] = [el("div", "control", el("span", "control-label", "Cycle"), actions)];
    if (locked) {
      rows.push(
        el("p", "hint warn", "Remote control is off on the machine. Press its Smart Control button to start or change it from here."),
      );
    }
    const options: [string, string, string, string, string][] = [
      ["custom.washerSpinLevel", "washerSpinLevel", "supportedWasherSpinLevel", "setWasherSpinLevel", "Spin"],
      ["custom.washerWaterTemperature", "washerWaterTemperature", "supportedWasherWaterTemperature", "setWasherWaterTemperature", "Water temperature"],
      ["custom.washerRinseCycles", "washerRinseCycles", "supportedWasherRinseCycles", "setWasherRinseCycles", "Rinses"],
      ["custom.washerSoilLevel", "washerSoilLevel", "supportedWasherSoilLevel", "setWasherSoilLevel", "Soil level"],
    ];
    for (const [optCap, attr, supportedAttr, command, label] of options) {
      const values = value(main, optCap, supportedAttr);
      if (!has(optCap) || !Array.isArray(values) || values.length === 0) continue;
      const select = el("select", "input");
      for (const v of values.map(String)) {
        const option = el("option", "", optCap === "custom.washerRinseCycles" ? v : valueLabel(v));
        option.value = v;
        option.selected = v === String(value(main, optCap, attr));
        select.append(option);
      }
      select.disabled = locked || machine === "run";
      select.addEventListener("change", () => void run(optCap, command, [select.value]));
      rows.push(el("label", "control inline", el("span", "control-label", label), select));
    }
    return el("section", "control-group", ...rows);
  }

  function energy(main: Main, has: (cap: string) => boolean): HTMLElement | null {
    if (!has("powerConsumptionReport")) return null;
    const report = value(main, "powerConsumptionReport", "powerConsumption") as { power?: number; energy?: number } | null;
    if (!report) return null;
    return el(
      "section",
      "energy",
      icon("energy", 14),
      el("span", "", `${report.power ?? 0} W now`),
      report.energy !== undefined ? el("span", "muted", `${(report.energy / 1000).toFixed(1)} kWh total`) : null,
    );
  }

  function everything(main: Main, disabled: Set<string>): HTMLElement | null {
    const rows: [string, string][] = [];
    for (const [cap, attrs] of Object.entries(main)) {
      if (HANDLED.has(cap) || disabled.has(cap)) continue;
      for (const [attr, entry] of Object.entries(attrs)) {
        const v = entry?.value;
        if (v === null || v === undefined || (typeof v === "object" && !Array.isArray(v))) continue;
        const text = Array.isArray(v) ? v.map((x) => (typeof x === "object" ? JSON.stringify(x) : String(x))).join(", ") : String(v);
        if (text) rows.push([`${cap} · ${attr}`, entry.unit ? `${text} ${entry.unit}` : text]);
      }
    }
    if (!rows.length) return null;
    return el(
      "details",
      "disclosure",
      el("summary", "", "Everything SmartThings reports"),
      el("dl", "facts mono", ...rows.flatMap(([k, v]) => [el("dt", "", k), el("dd", "", v)])),
    );
  }

  // Re-render only when the account or this device's link changes, not on every check result.
  const signature = () => JSON.stringify([account.connected, account.error, account.devices.length, current.cloudId]);
  let seen = signature();
  const unsubscribe = subscribe(() => {
    if (disposed) return;
    current = state.devices.find((d) => d.id === current.id) ?? current;
    const next = signature();
    if (next === seen) return;
    seen = next;
    status = null;
    render();
    void load();
  });
  render();
  void load();
  timer = window.setInterval(() => void load(true), POLL_MS);
  return () => {
    disposed = true;
    window.clearInterval(timer);
    unsubscribe();
  };
}

function value(main: Main, cap: string, attr: string): unknown {
  return main[cap]?.[attr]?.value ?? null;
}

function unitOf(main: Main, cap: string, attr: string): string {
  return main[cap]?.[attr]?.unit ?? "C";
}

/** Minutes until the cycle ends, from the remaining time or the completion time. */
function minutesLeft(main: Main, cap: string, machine: string): number | null {
  if (machine !== "run" && machine !== "pause") return null;
  const remaining = value(main, "samsungce.washerOperatingState", "remainingTime");
  if (typeof remaining === "number") return remaining;
  const completion = value(main, cap, "completionTime");
  if (typeof completion !== "string") return null;
  const ms = Date.parse(completion) - Date.now();
  return Number.isFinite(ms) && ms > 0 ? Math.round(ms / 60000) : null;
}

function progressBar(percent: number): HTMLElement {
  const fill = el("div", "progress-fill");
  fill.style.width = `${Math.max(0, Math.min(100, percent))}%`;
  return el("div", "progress small", fill);
}

function choice(
  label: string,
  options: string[],
  selected: string,
  onPick: (value: string) => Promise<void>,
  withIcons = false,
): HTMLElement {
  const group = el("div", "segmented");
  for (const option of options) {
    const iconName = withIcons ? MODE_ICONS[option] : undefined;
    const b = button(iconName ? [icon(iconName, 14), valueLabel(option)] : valueLabel(option), async () => {
      if (option === selected) return;
      for (const other of group.children) other.classList.remove("selected");
      b.classList.add("selected", "pending");
      await onPick(option);
    }, option === selected ? "segment selected" : "segment");
    group.append(b);
  }
  return el("div", "control", el("span", "control-label", label), group);
}

function callout(iconName: IconName, title: string, body: string, action: HTMLElement): HTMLElement {
  return el(
    "section",
    "callout",
    el("span", "kind-tile", icon(iconName, 20)),
    el("h3", "", title),
    el("p", "", body),
    action,
  );
}
