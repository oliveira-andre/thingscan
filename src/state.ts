// The home's devices and their latest reachability, shared by every view.
import * as api from "./api";
import type { Report, SavedDevice } from "./api";
import { toast } from "./toast";

export type Check = Report | "checking";

export const state = {
  devices: [] as SavedDevice[],
  checks: new Map<string, Check>(),
  checking: false,
  /** When the last full check finished, in ms. */
  lastCheck: null as number | null,
};

const listeners = new Set<() => void>();

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function notify(): void {
  for (const listener of listeners) {
    listener();
  }
}

/** Keeps this window in step with the others: device edits and check results made anywhere. */
export async function syncWithOtherWindows(): Promise<void> {
  await api.onDevicesChanged(() => void loadDevices());
  await api.onDeviceChecked(({ report, device }) => {
    if (device) {
      const index = state.devices.findIndex((d) => d.id === device.id);
      if (index !== -1) state.devices[index] = device;
    }
    state.checks.set(report.id, report);
    notify();
  });
  const snapshot = await api.lastReports();
  for (const report of snapshot.reports) {
    if (!state.checks.has(report.id)) state.checks.set(report.id, report);
  }
  if (snapshot.checkedAt && !state.lastCheck) state.lastCheck = snapshot.checkedAt;
  notify();
}

export async function loadDevices(): Promise<void> {
  state.devices = await api.listDevices();
  notify();
}

export function upsertDevice(device: SavedDevice): void {
  const index = state.devices.findIndex((d) => d.id === device.id);
  if (index === -1) {
    state.devices.push(device);
  } else {
    state.devices[index] = device;
  }
  notify();
}

export function dropDevice(id: string): void {
  state.devices = state.devices.filter((d) => d.id !== id);
  state.checks.delete(id);
  notify();
}

export function rooms(): string[] {
  return [...new Set(state.devices.map((d) => d.room).filter((r): r is string => !!r))].sort();
}

/** Checks that devices answer: all of them, or just `ids`. */
export async function checkDevices(ids?: string[]): Promise<void> {
  const targets = ids ?? state.devices.map((d) => d.id);
  if (targets.length === 0) {
    return;
  }
  const full = !ids;
  for (const id of targets) {
    state.checks.set(id, "checking");
  }
  if (full) {
    state.checking = true;
  }
  notify();
  try {
    await api.checkDevices(ids ?? null, ({ report, device }) => {
      if (device) {
        const index = state.devices.findIndex((d) => d.id === device.id);
        if (index !== -1) {
          state.devices[index] = device;
        }
      }
      state.checks.set(report.id, report);
      if (report.relocated && device) {
        toast("info", `${device.name} moved`, `It answers at ${report.address ?? "a new address"} now.`);
      }
      notify();
    });
  } catch (error) {
    toast("error", "Couldn't check devices", api.errorMessage(error));
  } finally {
    for (const id of targets) {
      if (state.checks.get(id) === "checking") {
        state.checks.delete(id);
      }
    }
    if (full) {
      state.checking = false;
      state.lastCheck = Date.now();
    }
    notify();
  }
}
