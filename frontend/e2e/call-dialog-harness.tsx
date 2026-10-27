import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { CallDialog } from "../src/features/chat/CallDialog";
import "../src/index.css";

/** Use the shipped playback UI rather than substituting a test-only player. */
export function mountCallDialog() {
  const container = document.createElement("div");
  document.body.append(container);
  createRoot(container).render(createElement(CallDialog));
}
