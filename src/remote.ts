// The TV remote: talks to a Samsung TV directly over its WebSocket API. The first time, the
// TV asks whether to allow Thingscan; after that it remembers.
import * as api from "./api";
import type { SavedDevice } from "./api";
import { button, el } from "./dom";
import { icon, type IconName } from "./icons";
import { toast } from "./toast";

type Phase = "connecting" | "allow" | "ready" | "off" | "error";

/** Tizen app ids. */
const APPS = [
  { name: "Netflix", id: "3201907018807" },
  { name: "YouTube", id: "111299001912" },
  { name: "Prime Video", id: "3201910019365" },
  { name: "Disney+", id: "3201901017640" },
];

/** Keyboard shortcuts while the remote is showing. */
const SHORTCUTS: Record<string, string> = {
  ArrowUp: "KEY_UP",
  ArrowDown: "KEY_DOWN",
  ArrowLeft: "KEY_LEFT",
  ArrowRight: "KEY_RIGHT",
  Enter: "KEY_ENTER",
  Backspace: "KEY_RETURN",
  h: "KEY_HOME",
  "+": "KEY_VOLUP",
  "=": "KEY_VOLUP",
  "-": "KEY_VOLDOWN",
  m: "KEY_MUTE",
  PageUp: "KEY_CHUP",
  PageDown: "KEY_CHDOWN",
};

/** Keys that repeat while held, as on a real remote: once on press, then again every
 * REPEAT_EVERY ms once held for REPEAT_AFTER. */
const REPEATING = new Set(["KEY_VOLUP", "KEY_VOLDOWN", "KEY_CHUP", "KEY_CHDOWN", "KEY_UP", "KEY_DOWN", "KEY_LEFT", "KEY_RIGHT"]);
const REPEAT_AFTER = 400;
const REPEAT_EVERY = 150;
/** Power held this long goes down on the TV too, pressed again every PRESS_EVERY ms. */
const POWER_HOLD_AFTER = 600;
const PRESS_EVERY = 120;
/** Scrolling over the touchpad sends a channel up or down per SCROLL_STEP px, at most every
 * SCROLL_EVERY ms. */
const SCROLL_STEP = 100;
const SCROLL_EVERY = 80;
/** Timers stop while the Mac sleeps: a gap of SLEPT_AFTER ms between ticks every TICK_EVERY
 * ms means it did. */
const TICK_EVERY = 5_000;
const SLEPT_AFTER = 30_000;

const sleep = (ms: number) => new Promise((r) => window.setTimeout(r, ms));

export function mountRemote(container: HTMLElement, device: SavedDevice): () => void {
  let phase: Phase = "connecting";
  let message = "";
  let disposed = false;
  const buttons = new Map<string, HTMLButtonElement>();

  const status = el("div", "remote-status");
  const pad = el("div", "remote");
  container.replaceChildren(status, pad);

  const setPhase = (next: Phase, text = "") => {
    phase = next;
    message = text;
    renderStatus();
    pad.classList.toggle("disabled", phase !== "ready");
  };

  const connect = async () => {
    setPhase(device.tvToken ? "connecting" : "allow");
    // Without a token the TV is showing its prompt; say so after a moment even with one.
    const hint = window.setTimeout(() => {
      if (phase === "connecting") setPhase("allow");
    }, 2500);
    try {
      await api.tvConnect(device.id);
      if (!disposed) setPhase("ready");
    } catch (error) {
      if (!disposed) failed(error);
    } finally {
      window.clearTimeout(hint);
    }
  };

  const wake = async () => {
    try {
      await api.tvWake(device.id);
      setPhase("connecting", "Waking the TV…");
      // A TV takes a few seconds to bring its network services up.
      for (let i = 0; i < 4 && !disposed && phase !== "ready"; i++) {
        await new Promise((r) => window.setTimeout(r, 3000));
        try {
          await api.tvConnect(device.id);
          if (!disposed) setPhase("ready");
        } catch {
          // still starting
        }
      }
      if (!disposed && phase !== "ready") {
        setPhase("off", "The TV didn't wake up. Turn on “Power on with mobile” in its settings, or switch it on by hand.");
      }
    } catch (error) {
      toast("error", "Couldn't wake the TV", api.errorMessage(error));
    }
  };

  // A TV that stops answering is off, which is also what switching it off from here looks like.
  const failed = (error: unknown) => {
    const text = api.errorMessage(error);
    setPhase(/may be off|didn't answer|couldn't reach/i.test(text) ? "off" : "error", text);
  };

  const send = async (key: string) => {
    if (phase !== "ready") return;
    flash(key);
    try {
      await api.tvKey(device.id, key);
    } catch (error) {
      failed(error);
    }
  };

  const flash = (key: string) => {
    const b = buttons.get(key);
    if (!b) return;
    b.classList.remove("pressed");
    void b.offsetWidth;
    b.classList.add("pressed");
  };

  // The button being held down, if any. Leaving the window or closing the remote lets go too,
  // since the pointer's own release may then never arrive.
  let held: { button: HTMLButtonElement; up: (released: boolean) => void } | null = null;
  const letGo = (released = false) => {
    const h = held;
    held = null;
    h?.up(released);
  };
  const onBlur = () => letGo();
  window.addEventListener("blur", onBlur);

  /** Calls `down` when the button is pressed, and `up` when it's let go (`released`) or the
   * pointer slides off it. */
  const onHold = (b: HTMLButtonElement, down: () => void, up: (released: boolean) => void) => {
    b.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || phase !== "ready") return;
      letGo();
      held = { button: b, up };
      down();
    });
    const end = (event: PointerEvent) => {
      if (held?.button === b) letGo(event.type === "pointerup");
    };
    b.addEventListener("pointerup", end);
    b.addEventListener("pointerleave", end);
    b.addEventListener("pointercancel", end);
  };

  const repeatWhileHeld = (b: HTMLButtonElement, k: string) => {
    let hold = 0;
    onHold(
      b,
      async () => {
        const mine = ++hold;
        await send(k);
        await sleep(REPEAT_AFTER);
        while (hold === mine && !disposed && phase === "ready") {
          await send(k);
          await sleep(REPEAT_EVERY);
        }
      },
      () => hold++,
    );
  };

  const key = (k: string, content: Node | string | (Node | string)[], className = "rkey", title?: string) => {
    const repeats = REPEATING.has(k);
    // A repeating key sends from pointerdown, so its click only counts from the keyboard.
    const b = button(content, (event) => {
      if (!repeats || event.detail === 0) void send(k);
    }, className);
    b.title = title ?? k.replace("KEY_", "").toLowerCase();
    buttons.set(k, b);
    if (repeats) repeatWhileHeld(b, k);
    return b;
  };
  const iconKey = (k: string, name: IconName, title: string, className = "rkey") => key(k, icon(name, 18), className, title);

  // Power: a click toggles (Art mode on a Frame TV). Held, the key goes down on the TV too and
  // is pressed again every PRESS_EVERY ms, as a real remote repeats a held key, until it's let
  // go; a few seconds of that switches a Frame TV fully off.
  const power = el("button", "rkey power", icon("power", 18));
  power.type = "button";
  power.title = "Power (hold to switch a Frame TV fully off)";
  buttons.set("KEY_POWER", power);
  let powerHold = 0;
  let powerTimer: number | undefined;
  let pressing: Promise<void> | null = null;
  onHold(
    power,
    () => {
      const mine = ++powerHold;
      powerTimer = window.setTimeout(() => {
        pressing = (async () => {
          while (powerHold === mine && !disposed) {
            flash("KEY_POWER");
            await api.tvPress(device.id, "KEY_POWER");
            await sleep(PRESS_EVERY);
          }
        })().catch(failed);
      }, POWER_HOLD_AFTER);
    },
    (released) => {
      window.clearTimeout(powerTimer);
      powerHold++;
      if (pressing) {
        // Let go once the last press has gone out.
        void pressing.then(() => (disposed ? undefined : api.tvRelease(device.id, "KEY_POWER"))).catch(() => {});
        pressing = null;
      } else if (released) {
        void send("KEY_POWER");
      }
    },
  );

  const rocker = (label: string, up: string, down: string) =>
    el(
      "div",
      "rocker",
      iconKey(up, "chevronUp", `${label} up`, "rkey rocker-key"),
      el("span", "rocker-label", label),
      iconKey(down, "chevronDown", `${label} down`, "rkey rocker-key"),
    );

  const dpad = el(
    "div",
    "dpad",
    iconKey("KEY_UP", "chevronUp", "Up", "rkey dpad-up"),
    iconKey("KEY_LEFT", "chevronLeft", "Left", "rkey dpad-left"),
    key("KEY_ENTER", "OK", "rkey dpad-ok", "OK"),
    iconKey("KEY_RIGHT", "chevronRight", "Right", "rkey dpad-right"),
    iconKey("KEY_DOWN", "chevronDown", "Down", "rkey dpad-down"),
  );

  // The touchpad moves the TV's pointer; a tap clicks where it is.
  const touchpad = el(
    "div",
    "touchpad",
    el("span", "touchpad-hint", "Drag to move the pointer", el("br"), "Tap to click", el("br"), "Scroll with two fingers"),
  );
  const ok = key("KEY_ENTER", "OK", "rkey touchpad-ok", "OK (select)");
  const touch = el("div", "touch-area", touchpad, ok);
  let mode: "buttons" | "touchpad" = loadMode();
  const center = el("div", "remote-center");
  const modeButtons = new Map<string, HTMLButtonElement>();
  const modes = el("div", "remote-modes");
  for (const [value, label] of [
    ["buttons", "Buttons"],
    ["touchpad", "Touchpad"],
  ] as const) {
    const b = button(label, () => setMode(value), "remote-mode");
    modeButtons.set(value, b);
    modes.append(b);
  }
  const setMode = (next: "buttons" | "touchpad") => {
    mode = next;
    saveMode(next);
    center.replaceChildren(next === "buttons" ? dpad : touch);
    for (const [value, b] of modeButtons) b.classList.toggle("selected", value === next);
  };
  setMode(mode);

  // Pointer moves are added up and sent every 40 ms, so a fast drag doesn't flood the TV.
  let pendingX = 0;
  let pendingY = 0;
  let flushing = false;
  const SPEED = 1.6;
  const flush = async () => {
    if (flushing) return;
    flushing = true;
    while (Math.round(pendingX) !== 0 || Math.round(pendingY) !== 0) {
      const dx = Math.round(pendingX);
      const dy = Math.round(pendingY);
      pendingX -= dx;
      pendingY -= dy;
      try {
        await api.tvPointer(device.id, dx, dy);
      } catch (error) {
        setPhase("error", api.errorMessage(error));
        pendingX = pendingY = 0;
        break;
      }
      await new Promise((r) => window.setTimeout(r, 40));
    }
    flushing = false;
  };
  let last: { x: number; y: number } | null = null;
  let travelled = 0;
  let downAt = 0;
  touchpad.addEventListener("pointerdown", (event) => {
    if (phase !== "ready") return;
    touchpad.setPointerCapture(event.pointerId);
    last = { x: event.clientX, y: event.clientY };
    travelled = 0;
    downAt = Date.now();
    touchpad.classList.add("active");
  });
  touchpad.addEventListener("pointermove", (event) => {
    if (!last) return;
    const dx = event.clientX - last.x;
    const dy = event.clientY - last.y;
    last = { x: event.clientX, y: event.clientY };
    travelled += Math.abs(dx) + Math.abs(dy);
    pendingX += dx * SPEED;
    pendingY += dy * SPEED;
    void flush();
  });
  const release = () => {
    if (!last) return;
    last = null;
    touchpad.classList.remove("active");
    if (travelled < 6 && Date.now() - downAt < 350) {
      touchpad.classList.add("clicked");
      window.setTimeout(() => touchpad.classList.remove("clicked"), 160);
      api.tvPointerClick(device.id).catch((error) => setPhase("error", api.errorMessage(error)));
    }
  };
  touchpad.addEventListener("pointerup", release);
  touchpad.addEventListener("pointercancel", release);

  // Channel up and down scroll the TV's browser, so a two-finger scroll over the touchpad sends
  // them. Only a few presses wait at a time, so a long fling doesn't keep scrolling after it.
  let pendingScroll = 0;
  let scrolling = false;
  const scroll = async () => {
    if (scrolling) return;
    scrolling = true;
    while (Math.abs(pendingScroll) >= SCROLL_STEP && phase === "ready" && !disposed) {
      const down = pendingScroll > 0;
      pendingScroll -= down ? SCROLL_STEP : -SCROLL_STEP;
      await send(down ? "KEY_CHDOWN" : "KEY_CHUP");
      await sleep(SCROLL_EVERY);
    }
    scrolling = false;
  };
  touchpad.addEventListener(
    "wheel",
    (event) => {
      event.preventDefault();
      if (phase !== "ready" || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      const dy = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaY * 16 : event.deltaY;
      pendingScroll = Math.max(-3 * SCROLL_STEP, Math.min(3 * SCROLL_STEP, pendingScroll + dy));
      void scroll();
    },
    { passive: false },
  );

  const numbers = el(
    "details",
    "disclosure remote-numbers",
    el("summary", "", "Number pad"),
    el(
      "div",
      "numpad",
      ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => key(`KEY_${n}`, String(n), "rkey num")),
      iconKey("KEY_CH_LIST", "list", "Channel list", "rkey num"),
      key("KEY_0", "0", "rkey num"),
      key("KEY_PRECH", "Prev", "rkey num", "Previous channel"),
    ),
  );

  const apps = el(
    "div",
    "remote-apps",
    ...APPS.map((app) =>
      button(app.name, async () => {
        if (phase !== "ready") return;
        try {
          await api.tvLaunch(device.id, app.id);
        } catch (error) {
          toast("error", `Couldn't open ${app.name}`, api.errorMessage(error));
        }
      }, "app-chip"),
    ),
  );

  pad.append(
    el("div", "remote-row", power, iconKey("KEY_SOURCE", "source", "Source"), iconKey("KEY_MUTE", "mute", "Mute")),
    modes,
    el("div", "remote-main", rocker("VOL", "KEY_VOLUP", "KEY_VOLDOWN"), center, rocker("CH", "KEY_CHUP", "KEY_CHDOWN")),
    el(
      "div",
      "remote-row",
      iconKey("KEY_RETURN", "backKey", "Back"),
      iconKey("KEY_HOME", "home", "Home"),
      iconKey("KEY_MENU", "menu", "Menu"),
    ),
    el(
      "div",
      "remote-row",
      iconKey("KEY_REWIND", "rewind", "Rewind"),
      iconKey("KEY_PLAY", "play", "Play"),
      iconKey("KEY_PAUSE", "pause", "Pause"),
      iconKey("KEY_FF", "fastForward", "Fast forward"),
    ),
    apps,
    numbers,
    el("p", "remote-hint", "Keyboard: arrows, Enter, Backspace for back, + and − for volume, M to mute, H for home."),
  );

  function renderStatus() {
    const retry = button([icon("refresh", 14), "Try again"], () => void connect(), "button-secondary button-small");
    const turnOn = button([icon("power", 14), "Turn on TV"], () => void wake(), "button-primary button-small");
    switch (phase) {
      case "connecting":
        status.className = "remote-status pending";
        status.replaceChildren(icon("spinner", 14), el("span", "", message || `Connecting to ${device.name}…`));
        break;
      case "allow":
        status.className = "remote-status pending";
        status.replaceChildren(
          icon("spinner", 14),
          el("span", "", "Look at the TV and choose ", el("strong", "", "Allow"), " so Thingscan can control it."),
        );
        break;
      case "ready":
        status.className = "remote-status ready";
        status.replaceChildren(el("span", "status-dot reachable"), el("span", "", `Connected to ${device.name}`));
        break;
      case "off":
        status.className = "remote-status problem";
        status.replaceChildren(icon("alert", 14), el("span", "", message || "The TV seems to be off."), turnOn, retry);
        break;
      case "error":
        status.className = "remote-status problem";
        status.replaceChildren(icon("alert", 14), el("span", "", message), retry);
        break;
    }
  }

  const onKey = (event: KeyboardEvent) => {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select") || event.metaKey || event.ctrlKey) return;
    const k = /^[0-9]$/.test(event.key) ? `KEY_${event.key}` : SHORTCUTS[event.key];
    if (k && phase === "ready") {
      event.preventDefault();
      void send(k);
    }
  };
  document.addEventListener("keydown", onKey);

  // Sleeping drops the TV's connection without the Mac noticing, and the first press after
  // waking would go nowhere. Reconnect as soon as the remote is showing again instead.
  let lastTick = Date.now();
  let slept = false;
  const reconnectIfSlept = () => {
    if (!slept || document.visibilityState !== "visible") return;
    slept = false;
    if (phase === "ready") {
      void api
        .tvDisconnect(device.id)
        .catch(() => {})
        .then(() => (disposed ? undefined : connect()));
    }
  };
  const ticking = window.setInterval(() => {
    const now = Date.now();
    if (now - lastTick > SLEPT_AFTER) slept = true;
    lastTick = now;
    reconnectIfSlept();
  }, TICK_EVERY);
  document.addEventListener("visibilitychange", reconnectIfSlept);

  void connect();

  return () => {
    disposed = true;
    letGo();
    window.clearInterval(ticking);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("keydown", onKey);
    document.removeEventListener("visibilitychange", reconnectIfSlept);
    void api.tvDisconnect(device.id);
  };
}

const MODE_KEY = "thingscan.remoteMode";

function loadMode(): "buttons" | "touchpad" {
  try {
    return localStorage.getItem(MODE_KEY) === "touchpad" ? "touchpad" : "buttons";
  } catch {
    return "buttons";
  }
}

function saveMode(mode: "buttons" | "touchpad"): void {
  try {
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    // Not remembering the mode is fine.
  }
}
