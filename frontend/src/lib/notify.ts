/**
 * Whether an inbound message should raise a desktop notification (pure — unit-testable).
 * Suppress only when the user is already looking at that conversation in a focused window;
 * otherwise (window unfocused, or a different conversation open) notify.
 */
export function shouldNotify(opts: {
  windowFocused: boolean;
  isActiveConversation: boolean;
}): boolean {
  return !(opts.windowFocused && opts.isActiveConversation);
}

let granted: boolean | null = null;

/**
 * Request notification permission once, eagerly, at startup.
 *
 * On macOS the dock-icon unread badge is gated by the app's notification authorization
 * (System Settings ▸ Notifications ▸ <app> ▸ Badges). Until the app has requested and been
 * granted that authorization, `setBadgeLabel` silently no-ops and no dock number ever appears.
 * `notifyInbound` only requests lazily (when a notification would actually fire), so a user who
 * is always focused on the active chat would never register — and never get a dock badge.
 * Calling this on startup ensures the badge works out of the box. Safe to call repeatedly.
 */
export async function ensureNotificationPermission(): Promise<void> {
  try {
    const n = await import("@tauri-apps/plugin-notification");
    if (granted === null) {
      granted = await n.isPermissionGranted();
      if (!granted) granted = (await n.requestPermission()) === "granted";
    }
  } catch {
    /* best-effort — notifications/badges are non-critical */
  }
}

/** Best-effort desktop notification for an inbound message. */
export async function notifyInbound(
  title: string,
  body: string,
  isActiveConversation: boolean,
): Promise<void> {
  try {
    const windowFocused =
      typeof document !== "undefined" ? document.hasFocus() : false;
    if (!shouldNotify({ windowFocused, isActiveConversation })) return;
    // Lazy import so this module (and the store that uses it) stays importable in the node
    // unit-test environment, where the Tauri plugin has no runtime.
    const n = await import("@tauri-apps/plugin-notification");
    if (granted === null) {
      granted = await n.isPermissionGranted();
      if (!granted) granted = (await n.requestPermission()) === "granted";
    }
    if (granted) n.sendNotification({ title, body: body || "New message" });
  } catch {
    /* notifications are best-effort — never let them break message handling */
  }
}
