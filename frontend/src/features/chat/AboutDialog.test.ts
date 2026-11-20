// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const { openUrl } = vi.hoisted(() => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));
vi.mock("@/lib/api", () => ({
  obs: { envInfo: vi.fn(async () => ({ app_version: "0.4.6-dev" })) },
}));
vi.mock("./UpdatePanel", () => ({ UpdatePanel: () => null }));
import { AboutDialog } from "./AboutDialog";

it("credits the bundled emoji artwork and opens its license on request", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () => {
      root.render(
        createElement(AboutDialog, { open: true, hideTrigger: true }),
      );
    });
    const dialog = document.querySelector('[data-testid="about-dialog"]')!;
    expect(dialog.textContent).toContain("Twemoji © Twitter and contributors");
    const license = Array.from(dialog.querySelectorAll("button")).find(
      (button) => button.textContent === "CC BY 4.0",
    );
    expect(license).toBeDefined();
    expect(openUrl).not.toHaveBeenCalled();
    await act(async () => license!.click());
    expect(openUrl).toHaveBeenCalledWith(
      "https://creativecommons.org/licenses/by/4.0/",
    );
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});
