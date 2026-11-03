// Notices in the bottom-right corner.
import { el } from "./dom";
import { icon } from "./icons";

export type ToastTone = "info" | "success" | "error";

export function toast(tone: ToastTone, title: string, detail?: string): void {
  const host = document.querySelector<HTMLElement>("#toasts")!;
  const node = el(
    "div",
    `toast ${tone}`,
    el("span", "toast-mark"),
    el("div", "toast-body", el("div", "toast-title", title), detail ? el("div", "toast-detail", detail) : null),
  );
  const close = el("button", "toast-close");
  close.type = "button";
  close.setAttribute("aria-label", "Dismiss");
  close.append(icon("close", 14));
  node.append(close);
  host.append(node);

  let timer: number | undefined;
  const remove = () => {
    window.clearTimeout(timer);
    node.classList.add("leaving");
    window.setTimeout(() => node.remove(), 160);
  };
  close.addEventListener("click", remove);
  // Errors stay until dismissed; they usually need reading.
  if (tone !== "error") {
    timer = window.setTimeout(remove, tone === "success" ? 3500 : 5000);
  }
}
