// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { GoogleDriveConnection } from "./GoogleDriveConnection";
const api = vi.hoisted(() => ({
  connect: vi.fn(),
  disconnect: vi.fn(),
  cancel: vi.fn(),
  available: vi.fn(),
}));
vi.mock("@/lib/api/googleDrive", () => ({ googleDrive: api }));
let root: Root;
let host: HTMLDivElement;
beforeEach(async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  vi.resetAllMocks();
  api.available.mockResolvedValue(true);
  api.disconnect.mockResolvedValue(undefined);
  api.cancel.mockResolvedValue(undefined);
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function click(label: string) {
  const button = [...host.querySelectorAll("button")].find(
    (b) => b.textContent === label,
  );
  expect(button).toBeDefined();
  await act(async () => button!.click());
}
it("connects only after an explicit click and disconnects on unmount", async () => {
  api.connect.mockResolvedValue(undefined);
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        onConnected: vi.fn(),
        connectionId: "view-1",
      }),
    ),
  );
  expect(api.connect).not.toHaveBeenCalled();
  await click("Connect Google Drive");
  expect(api.connect).toHaveBeenCalledWith("view-1");
  await act(async () => root.unmount());
  expect(api.disconnect).toHaveBeenCalledWith("view-1");
  root = createRoot(host);
});
it("explains an unconfigured build without opening Google", async () => {
  api.available.mockResolvedValue(false);
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        onConnected: vi.fn(),
        connectionId: "view-2",
      }),
    ),
  );
  expect(host.textContent).toContain(
    "Google Drive is not configured in this build",
  );
  expect(api.connect).not.toHaveBeenCalled();
});
it("does not publish a late connection after the view is retired", async () => {
  let finish!: () => void;
  api.connect.mockImplementation(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        onConnected,
        connectionId: "view-3",
      }),
    ),
  );
  await click("Connect Google Drive");
  await act(async () => root.unmount());
  await act(async () => finish());
  expect(onConnected).not.toHaveBeenCalled();
  expect(api.disconnect).toHaveBeenCalledWith("view-3");
  root = createRoot(host);
});
it("offers retry after authorization failure", async () => {
  api.connect
    .mockRejectedValueOnce(new Error("Google authorization was cancelled"))
    .mockResolvedValueOnce(undefined);
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        onConnected,
        connectionId: "view-4",
      }),
    ),
  );
  await click("Connect Google Drive");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "cancelled",
  );
  await click("Connect Google Drive");
  expect(onConnected).toHaveBeenCalledOnce();
});
