// Labels, icons and times shown for devices.
import type { Kind, ScanChannel, Source } from "./api";
import type { IconName } from "./icons";

const KIND_LABELS: Record<Kind, string> = {
  ready_to_add: "Ready to add",
  hub: "Hub",
  tv: "TV",
  appliance: "Appliance",
  matter: "Matter device",
  thread_border_router: "Thread border router",
  tracker: "Tracker",
  samsung: "Samsung device",
  other: "Device",
};

export const kindLabel = (kind: Kind) => KIND_LABELS[kind];

const SOURCE_LABELS: Record<Source, string> = {
  ocf: "OCF",
  ssdp: "UPnP",
  mdns: "mDNS",
  ble: "Bluetooth",
  softap: "Wi-Fi setup",
};

export const sourceLabel = (source: Source) => SOURCE_LABELS[source];

export const CHANNELS: { channel: ScanChannel; label: string; title: string }[] = [
  { channel: "ocf", label: "OCF", title: "Samsung TVs and appliances over CoAP" },
  { channel: "ssdp", label: "UPnP", title: "TVs and media devices (SSDP)" },
  { channel: "mdns", label: "Bonjour", title: "SmartThings hubs, Matter and Thread (mDNS)" },
  { channel: "ble", label: "Bluetooth", title: "Devices in setup mode, SmartTags, Matter" },
  { channel: "wifi", label: "Wi-Fi setup", title: "Setup networks opened by new devices" },
];

/** The best icon for a device, from its OCF type, Matter type or name, then its kind. */
export function deviceIcon(kind: Kind, name: string, details: Record<string, string>): IconName {
  const hints = [
    name,
    detail(details, "ocf.device_types"),
    detail(details, "matter.device_type"),
    details["cloud.category"],
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  const rules: [RegExp, IconName][] = [
    [/airconditioner|air.?condition|a\/c\b|\bac\b/, "airVent"],
    [/washer|dryer|laundry|clothingcare/, "washer"],
    [/refrigerator|fridge/, "fridge"],
    [/oven|cooktop|range|microwave|dishwasher/, "cooking"],
    [/robot|vacuum|cleaner/, "bot"],
    [/thermostat|temperature/, "thermometer"],
    [/purifier|fan/, "fan"],
    [/light|lamp|bulb/, "light"],
    [/plug|outlet|switch/, "plug"],
    [/lock/, "lock"],
    [/speaker/, "speaker"],
    [/\btv\b|television|oic\.d\.tv|video player/, "tv"],
    [/\bhub\b|bridge/, "router"],
  ];
  for (const [pattern, iconName] of rules) {
    if (pattern.test(hints)) {
      return iconName;
    }
  }
  const byKind: Record<Kind, IconName> = {
    ready_to_add: "sparkles",
    hub: "router",
    tv: "tv",
    appliance: "chip",
    matter: "chip",
    thread_border_router: "radioTower",
    tracker: "tag",
    samsung: "bluetooth",
    other: "chip",
  };
  return byKind[kind];
}

/** "just now", "5 min ago", "3 h ago", "2 days ago". */
export function ago(seconds: number): string {
  const delta = Math.max(0, Date.now() / 1000 - seconds);
  if (delta < 45) return "just now";
  if (delta < 3600) return `${Math.round(delta / 60)} min ago`;
  if (delta < 86400) return `${Math.round(delta / 3600)} h ago`;
  const days = Math.round(delta / 86400);
  return `${days} day${days === 1 ? "" : "s"} ago`;
}

export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function date(seconds: number): string {
  return new Date(seconds * 1000).toLocaleDateString([], { year: "numeric", month: "short", day: "numeric" });
}

export function makeAndModel(manufacturer?: string | null, model?: string | null): string {
  return [manufacturer, model].filter((p): p is string => !!p).join(" · ");
}

/** A scan detail, also when merging prefixed it with its channel (`BLE.st.mnid`). */
export function detail(details: Record<string, string>, key: string): string | undefined {
  return details[key] ?? Object.entries(details).find(([k]) => k.endsWith(`.${key}`))?.[1];
}

/** The line under a device's name: its maker and model, or what its advert says it is. */
export function subtitle(device: {
  kind: Kind;
  manufacturer?: string | null;
  model?: string | null;
  details: Record<string, string>;
}): string {
  const made = makeAndModel(device.manufacturer, device.model);
  if (made) return made;
  const mnid = detail(device.details, "st.mnid");
  if (mnid) return `SmartThings device · model ${mnid}`;
  const vendor = detail(device.details, "matter.vendor_id");
  if (vendor) return `Matter · vendor ${vendor}`;
  return kindLabel(device.kind);
}

/** The app's kind for a SmartThings category (matches kind_of_category in store.rs). */
export function kindOfCategory(category: string | null): Kind {
  switch (category) {
    case "Television":
    case "TV":
      return "tv";
    case "Hub":
    case "Bridges":
      return "hub";
    case "AirConditioner":
    case "Washer":
    case "Dryer":
    case "Refrigerator":
    case "Dishwasher":
    case "Oven":
    case "Range":
    case "Microwave":
    case "RobotCleaner":
    case "AirPurifier":
    case "Humidifier":
    case "Dehumidifier":
    case "Cooktop":
    case "KimchiRefrigerator":
    case "WaterHeater":
    case "ClothingCareMachine":
      return "appliance";
    default:
      return "other";
  }
}

/** "AirConditioner" → "Air conditioner". */
export function categoryLabel(category: string | null): string {
  if (!category) return "Device";
  const words = category.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const VALUE_LABELS: Record<string, string> = {
  wind: "Fan",
  windFree: "WindFree",
  aIComfort: "AI comfort",
  auto: "Auto",
  fixed: "Fixed",
  all: "All",
  vertical: "Up & down",
  horizontal: "Side to side",
  indirect: "Indirect",
  direct: "Direct",
  run: "Start",
  pause: "Pause",
  stop: "Stop",
  none: "None",
  noSpin: "No spin",
  tapCold: "Tap cold",
};

/** A readable label for an enum value from SmartThings: "extraHigh" → "Extra high". */
export function valueLabel(value: unknown): string {
  const text = String(value);
  if (VALUE_LABELS[text]) return VALUE_LABELS[text];
  const words = text.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/_/g, " ").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}
