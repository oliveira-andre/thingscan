// Updates a list in place: an item keeps its node while its signature is unchanged, so
// streaming updates don't flicker hover states or reset focus.

interface Entry {
  signature: string;
  node: HTMLElement;
}

const caches = new WeakMap<HTMLElement, Map<string, Entry>>();

export function keyedList<T>(
  container: HTMLElement,
  items: T[],
  key: (item: T) => string,
  signature: (item: T) => string,
  build: (item: T) => HTMLElement,
): HTMLElement[] {
  const cache = caches.get(container) ?? new Map<string, Entry>();
  const next = new Map<string, Entry>();
  const nodes = items.map((item) => {
    const k = key(item);
    const sig = signature(item);
    const hit = cache.get(k);
    const node = hit && hit.signature === sig ? hit.node : build(item);
    next.set(k, { signature: sig, node });
    return node;
  });
  caches.set(container, next);
  const unchanged = nodes.length === container.children.length && nodes.every((n, i) => container.children[i] === n);
  if (!unchanged) {
    container.replaceChildren(...nodes);
  }
  return nodes;
}
