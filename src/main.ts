import "./fonts";
import { loadAccount, mountAccountMenu } from "./account";
import { errorMessage } from "./api";
import { mountAdd } from "./add";
import { mountHome } from "./home";
import { currentView, go, register } from "./router";
import { listen } from "@tauri-apps/api/event";
import { checkDevices, loadDevices, state, syncWithOtherWindows } from "./state";
import { toast } from "./toast";

/** Coming back to the window after this long checks every device again. */
const RECHECK_AFTER_MS = 5 * 60_000;

register("home", mountHome);
register("add", mountAdd);
mountAccountMenu(document.querySelector<HTMLElement>("#account-menu")!);

for (const link of document.querySelectorAll<HTMLElement>(".nav-link")) {
  link.addEventListener("click", () => {
    const view = link.dataset.view === "add" ? "add" : "home";
    if (currentView() !== view) go(view);
  });
}

window.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && currentView() === "add" && !document.querySelector(".backdrop, .drawer")) {
    go("home");
  }
});

window.addEventListener("focus", () => {
  if (!state.checking && state.lastCheck && Date.now() - state.lastCheck > RECHECK_AFTER_MS) {
    void checkDevices();
  }
});

// Asked for by the menu bar panel or its menu.
void listen<string>("navigate", (event) => go(event.payload === "add" ? "add" : "home"));
void listen("check-all", () => {
  if (!state.checking) void checkDevices();
});

async function boot(): Promise<void> {
  go("home");
  void syncWithOtherWindows();
  try {
    await loadDevices();
  } catch (error) {
    toast("error", "Couldn't load your devices", errorMessage(error));
    return;
  }
  // Opening the app makes sure every added device still answers. The SmartThings account
  // loads alongside, linking devices to it for their controls.
  await Promise.all([checkDevices(), loadAccount()]);
}

void boot();
