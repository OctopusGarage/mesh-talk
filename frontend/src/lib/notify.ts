let granted: boolean | null = null;
let permissionCheck: Promise<boolean> | null = null;

function notificationPermission(): Promise<boolean> {
  if (permissionCheck) return permissionCheck;
  if (granted !== null) return Promise.resolve(granted);
  const pending = (async () => {
    const n = await import("@tauri-apps/plugin-notification");
    granted = await n.isPermissionGranted();
    if (!granted) granted = (await n.requestPermission()) === "granted";
    return granted;
  })();
  permissionCheck = pending.then(
    (allowed) => {
      permissionCheck = null;
      return allowed;
    },
    (error) => {
      permissionCheck = null;
      throw error;
    },
  );
  return permissionCheck;
}

/**
 * Request notification permission once, eagerly, at startup.
 *
 * On macOS the dock-icon unread badge is gated by the app's notification authorization
 * (System Settings ▸ Notifications ▸ <app> ▸ Badges). Until the app has requested and been
 * granted that authorization, `setBadgeLabel` silently no-ops and no dock number ever appears.
 * The native backend owns notification delivery; this startup request gives the Dock badge
 * authorization even if no message has arrived yet. Safe to call repeatedly.
 */
export async function ensureNotificationPermission(): Promise<void> {
  try {
    await notificationPermission();
  } catch {
    /* best-effort — notifications/badges are non-critical */
  }
}
