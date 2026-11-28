import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

let minimized = false;
const observers = new Set<() => void>();

export function isNativeWindowMinimized() {
  return minimized;
}

export function subscribeNativeWindowMinimized(observer: () => void) {
  observers.add(observer);
  return () => {
    observers.delete(observer);
  };
}

export function startNativeWindowVisibility() {
  if (!isTauri()) return;
  let active = true;
  let seenEvent = false;
  const update = (value: boolean) => {
    if (!active || minimized === value) return;
    minimized = value;
    for (const observer of observers) observer();
  };
  const subscription = listen<boolean>(
    "main-window-minimized",
    ({ payload }) => {
      seenEvent = true;
      update(payload);
    },
  );
  void subscription
    .then(async () => {
      if (!active || seenEvent) return;
      const current = await getCurrentWindow().isMinimized();
      if (!seenEvent) update(current);
    })
    .catch(() => {});
  return () => {
    active = false;
    void subscription.then((unlisten) => unlisten()).catch(() => {});
  };
}
