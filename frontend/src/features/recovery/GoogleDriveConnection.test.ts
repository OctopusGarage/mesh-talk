// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { GoogleDriveConnection } from "./GoogleDriveConnection";
const api = vi.hoisted(() => ({
  connect: vi.fn(),
  resume: vi.fn(),
  forget: vi.fn(),
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
  api.resume.mockResolvedValue(false);
  api.forget.mockResolvedValue(undefined);
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

it("silently resumes a remembered grant without opening Google sign-in", async () => {
  api.resume.mockResolvedValue(true);
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "remembered",
        onConnected,
      }),
    ),
  );
  expect(api.resume).toHaveBeenCalledWith("remembered");
  expect(api.connect).not.toHaveBeenCalled();
  expect(onConnected).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("Google Drive connected");
});
it("keeps sign-in available after an expired remembered grant", async () => {
  api.resume.mockRejectedValue(
    new Error("Google access expired. Sign in again."),
  );
  api.connect.mockResolvedValue(undefined);
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "expired",
        onConnected,
      }),
    ),
  );
  await click("Connect Google Drive");
  expect(onConnected).toHaveBeenCalledOnce();
});
it("does not publish a resumed connection after unmount", async () => {
  let finish!: (connected: boolean) => void;
  api.resume.mockImplementation(
    () =>
      new Promise<boolean>((resolve) => {
        finish = resolve;
      }),
  );
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "retired",
        onConnected,
      }),
    ),
  );
  await act(async () => root.unmount());
  await act(async () => finish(true));
  expect(onConnected).not.toHaveBeenCalled();
  root = createRoot(host);
});

it("forgets the remembered grant only after an explicit disconnect action", async () => {
  api.resume.mockResolvedValue(true);
  const onDisconnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "remembered",
        onConnected: vi.fn(),
        onDisconnected,
      }),
    ),
  );
  expect(api.forget).not.toHaveBeenCalled();
  await click("Disconnect / change Google account");
  expect(api.forget).toHaveBeenCalledWith("remembered");
  expect(onDisconnected).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("Connect Google Drive");
});

it("does not forget saved authorization when a view closes", async () => {
  api.resume.mockResolvedValue(true);
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "remembered",
        onConnected: vi.fn(),
      }),
    ),
  );
  await act(async () => root.unmount());
  expect(api.disconnect).toHaveBeenCalledWith("remembered");
  expect(api.forget).not.toHaveBeenCalled();
  root = createRoot(host);
});

it("retires a new sign-in after a cancelled attempt even when the view unmounts", async () => {
  let finish!: () => void;
  api.connect
    .mockImplementationOnce(() => new Promise<void>(() => {}))
    .mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "retry-retired",
        onConnected,
      }),
    ),
  );
  await click("Connect Google Drive");
  await click("Cancel");
  await click("Connect Google Drive");
  await act(async () => root.unmount());
  await act(async () => finish());
  expect(onConnected).not.toHaveBeenCalled();
  root = createRoot(host);
});

it("rechecks availability after a failed availability request", async () => {
  api.available
    .mockRejectedValueOnce(new Error("Native host unavailable"))
    .mockResolvedValue(true);
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "availability-retry",
        onConnected: vi.fn(),
      }),
    ),
  );
  await click("Retry");
  expect(api.available).toHaveBeenCalledTimes(2);
  expect(
    [...host.querySelectorAll("button")].find(
      (button) => button.textContent === "Connect Google Drive",
    )?.disabled,
  ).toBe(false);
});

it("retries remembered authorization after a network error without opening Google sign-in", async () => {
  api.resume
    .mockRejectedValueOnce(new Error("Network unavailable"))
    .mockResolvedValue(true);
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "network-retry",
        onConnected,
      }),
    ),
  );
  await click("Retry");
  expect(api.resume).toHaveBeenCalledTimes(2);
  expect(api.connect).not.toHaveBeenCalled();
  expect(onConnected).toHaveBeenCalledOnce();
});
