// Two screens: the home and "Add device". Each mount returns its unmount.
export type View = "home" | "add";
type Mount = (root: HTMLElement) => () => void;

const views = new Map<View, Mount>();
let current: View | null = null;
let unmount: (() => void) | null = null;

export function register(view: View, mount: Mount): void {
  views.set(view, mount);
}

export function go(view: View): void {
  unmount?.();
  const root = document.querySelector<HTMLElement>("#view")!;
  root.scrollTop = 0;
  current = view;
  for (const link of document.querySelectorAll<HTMLElement>(".nav-link")) {
    link.classList.toggle("active", link.dataset.view === view);
  }
  unmount = views.get(view)!(root);
}

export const currentView = () => current;
