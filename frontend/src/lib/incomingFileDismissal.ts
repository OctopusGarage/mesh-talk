// Explicit dismissal differs from eviction by the received-tray size limit.
const listeners = new Set<(fileConv: string) => void>();
export function notifyIncomingFileDismissed(fileConv: string) {
  listeners.forEach((listener) => listener(fileConv));
}
export function onIncomingFileDismissed(listener: (fileConv: string) => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
