import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
// Bundle fonts LOCALLY (offline desktop app, CSP-safe — no CDN). Variable packages.
import "@fontsource-variable/space-grotesk"; // display
import "@fontsource-variable/inter"; // body
import "@fontsource-variable/geist-mono"; // mono (crypto/identity/ports/IDs)
import "./lib/theme"; // apply the persisted theme before first paint
import "./lib/i18n"; // initialize i18next before first paint
import { applyPlatformClass } from "./lib/platform";
import "./index.css";

// Mark the root with `data-os="macos"` under the overlay title bar so the sidebar header
// gets traffic-light clearance (macOS only; no-op elsewhere). Runs before first paint.
applyPlatformClass();

// Desktop app: suppress the webview's native right-click context menu (Cut/Copy/Inspect…)
// in production builds so it doesn't behave like a browser. Kept in dev for debugging.
// Exception: regions that opt into a custom in-app menu (marked `[data-context-menu]`,
// e.g. message bubbles) keep their right-click so our own menu can open there.
if (import.meta.env.PROD) {
  document.addEventListener("contextmenu", (e) => {
    if ((e.target as Element | null)?.closest("[data-context-menu]")) return;
    e.preventDefault();
  });
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
