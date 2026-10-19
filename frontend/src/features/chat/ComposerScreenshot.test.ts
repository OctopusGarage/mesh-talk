// @vitest-environment jsdom
import { act, createElement, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { Composer } from "./Composer";

let root: Root;
let container: HTMLDivElement;
const paste = vi.fn();
const send = vi.fn(() => true);
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("URL", {
    createObjectURL: () => "blob:preview",
    revokeObjectURL: vi.fn(),
  });
  paste.mockReset().mockResolvedValue(undefined);
  send.mockClear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});
async function render(stagedScreenshot: File | null) {
  await act(async () =>
    root.render(
      createElement(
        StrictMode,
        null,
        createElement(Composer, {
          onSend: send,
          onPasteFiles: paste,
          stagedScreenshot,
          placeholder: "Message",
          mentionNames: [],
        }),
      ),
    ),
  );
}
it("stages a confirmed screenshot once in StrictMode and sends only on explicit Send", async () => {
  const file = new File(["png"], "screenshot.png", { type: "image/png" });
  await render(file);
  await render(file);
  expect(
    container.querySelectorAll(
      '[data-testid="composer-pending-files"] [title="screenshot.png"]',
    ),
  ).toHaveLength(1);
  expect(document.activeElement).toBe(container.querySelector("textarea"));
  expect(
    container.querySelector('[data-testid="composer-pending-image"]'),
  ).not.toBeNull();
  expect(paste).not.toHaveBeenCalled();
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  expect(paste).toHaveBeenCalledExactlyOnceWith([file]);
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).toBeNull();
});
it("drops staged screenshots when their ownership is invalidated", async () => {
  await render(new File(["one"], "screenshot.png", { type: "image/png" }));
  await render(new File(["two"], "screenshot.png", { type: "image/png" }));
  await render(null);
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).toBeNull();
  expect(paste).not.toHaveBeenCalled();
});
it("keeps the screenshot staged when sending fails", async () => {
  paste.mockRejectedValueOnce(new Error("disk full"));
  await render(new File(["png"], "screenshot.png", { type: "image/png" }));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).not.toBeNull();
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  expect(paste).toHaveBeenCalledTimes(2);
});

it("dismisses a screenshot without restaging it on the next render", async () => {
  const file = new File(["png"], "screenshot.png", { type: "image/png" });
  await render(file);
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>(
        '[aria-label="Dismiss screenshot.png"]',
      )!
      .click(),
  );
  await render(file);
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).toBeNull();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  expect(paste).not.toHaveBeenCalled();
});

it("keeps ordinary copied files when staged screenshot ownership is invalidated", async () => {
  await render(new File(["png"], "screenshot.png", { type: "image/png" }));
  const event = new Event("paste", { bubbles: true });
  Object.defineProperty(event, "clipboardData", {
    value: {
      files: [new File(["report"], "report.txt", { type: "text/plain" })],
      items: [],
    },
  });
  await act(async () =>
    container.querySelector("textarea")!.dispatchEvent(event),
  );
  await render(null);
  expect(
    container.querySelector('[data-testid="composer-pending-files"]')
      ?.textContent,
  ).toContain("report.txt");
  expect(container.querySelector('[title="screenshot.png"]')).toBeNull();
});
