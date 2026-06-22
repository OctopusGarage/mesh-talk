// Lightweight, dependency-free platform detection for chrome adjustments.
//
// The custom title bar (tauri.conf `titleBarStyle: "Overlay"`) is macOS-only: the native
// traffic-lights float over the top-left of our content, so the sidebar header needs a
// top/left inset there — and ONLY there. On Windows/Linux the native title bar is kept,
// so no inset (it would just be awkward empty space). In a plain browser (the Playwright
// e2e harness, dev in a tab) there are no traffic-lights either, so no inset.
//
// We mark the document root with `data-os="macos"` only when BOTH: we're inside the Tauri
// webview (`__TAURI_INTERNALS__`) AND the platform looks like a Mac. CSS keys off it.

function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>)
  );
}

function looksLikeMac(): boolean {
  if (typeof navigator === "undefined") return false;
  // The macOS WebView (WKWebView) user-agent always contains "Macintosh" / "Mac OS X".
  // (We avoid the deprecated `navigator.platform`.)
  return /Mac OS X|Macintosh/i.test(navigator.userAgent || "");
}

/** True when the macOS overlay title bar is in effect (Tauri + Mac). */
export function isMacOverlay(): boolean {
  return isTauri() && looksLikeMac();
}

/** Set `data-os` on <html> so CSS can apply the traffic-light inset only on macOS overlay. */
export function applyPlatformClass(): void {
  if (typeof document === "undefined") return;
  if (isMacOverlay()) {
    document.documentElement.setAttribute("data-os", "macos");
  }
}
