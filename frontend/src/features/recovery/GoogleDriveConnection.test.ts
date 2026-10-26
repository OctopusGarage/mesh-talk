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

it("keeps the grant connected after forgetting fails and allows retry", async () => {
  api.resume.mockResolvedValue(true);
  api.forget.mockRejectedValueOnce(new Error("Could not forget account"));
  const onDisconnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "forget-retry",
        onConnected: vi.fn(),
        onDisconnected,
      }),
    ),
  );
  await click("Disconnect / change Google account");
  expect(host.textContent).toContain("Could not forget account");
  expect(host.textContent).toContain("Google Drive connected");
  expect(onDisconnected).not.toHaveBeenCalled();
  await click("Disconnect / change Google account");
  expect(onDisconnected).toHaveBeenCalledOnce();
});
it("reports a failed authorization cancellation and releases the sign-in controls", async () => {
  api.connect.mockImplementationOnce(() => new Promise<void>(() => {}));
  api.disconnect.mockRejectedValueOnce(new Error("Cancellation failed"));
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "cancel-failure",
        onConnected: vi.fn(),
      }),
    ),
  );
  await click("Connect Google Drive");
  await click("Cancel");
  expect(host.textContent).toContain("Cancellation failed");
  expect(
    [...host.querySelectorAll("button")].find(
      (item) => item.textContent === "Connect Google Drive",
    )?.disabled,
  ).toBe(false);
});
it("disables authorization and account removal while the parent owns a transfer", async () => {
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "disabled",
        onConnected: vi.fn(),
        disabled: true,
      }),
    ),
  );
  await click("Connect Google Drive");
  expect(api.connect).not.toHaveBeenCalled();
  api.resume.mockResolvedValue(true);
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "disabled-connected",
        onConnected: vi.fn(),
        disabled: true,
      }),
    ),
  );
  await click("Disconnect / change Google account");
  expect(api.forget).not.toHaveBeenCalled();
});
it("ignores late availability after the connection view is replaced", async () => {
  let finish!: (value: boolean) => void;
  api.available.mockReturnValueOnce(
    new Promise<boolean>((resolve) => {
      finish = resolve;
    }),
  );
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "old-view",
        onConnected,
      }),
    ),
  );
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "new-view",
        onConnected,
      }),
    ),
  );
  await act(async () => finish(true));
  expect(api.resume).toHaveBeenCalledExactlyOnceWith("new-view");
  expect(onConnected).not.toHaveBeenCalled();
});
it.each(["resolve", "reject"] as const)(
  "retires a pending forget %s when its connection view is replaced",
  async (outcome) => {
    let finish!: () => void;
    let fail!: (error: Error) => void;
    api.resume.mockResolvedValue(true);
    api.forget.mockReturnValueOnce(
      new Promise<void>((resolve, reject) => {
        finish = resolve;
        fail = reject;
      }),
    );
    const onDisconnected = vi.fn();
    const onConnected = vi.fn();
    await act(async () =>
      root.render(
        createElement(GoogleDriveConnection, {
          connectionId: "old",
          onConnected,
          onDisconnected,
        }),
      ),
    );
    const disconnect = [...host.querySelectorAll("button")].find(
      (item) => item.textContent === "Disconnect / change Google account",
    )!;
    await act(async () => {
      disconnect.click();
      disconnect.click();
    });
    expect(api.forget).toHaveBeenCalledOnce();
    await act(async () =>
      root.render(
        createElement(GoogleDriveConnection, {
          connectionId: "new",
          onConnected,
          onDisconnected,
        }),
      ),
    );
    await act(async () =>
      outcome === "resolve" ? finish() : fail(new Error("old forget failed")),
    );
    expect(onDisconnected).not.toHaveBeenCalled();
    expect(host.textContent).not.toContain("old forget failed");
    expect(host.textContent).toContain("Google Drive connected");
  },
);
it("admits one authorization request before React commits the busy state", async () => {
  api.connect.mockResolvedValue(undefined);
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "double-click",
        onConnected,
      }),
    ),
  );
  const connect = [...host.querySelectorAll("button")].find(
    (item) => item.textContent === "Connect Google Drive",
  )!;
  await act(async () => {
    connect.click();
    connect.click();
  });
  expect(api.connect).toHaveBeenCalledOnce();
  expect(onConnected).toHaveBeenCalledOnce();
});
it("ignores a late availability failure after its connection view is replaced", async () => {
  let fail!: (error: Error) => void;
  api.available.mockReturnValueOnce(
    new Promise<boolean>((_resolve, reject) => {
      fail = reject;
    }),
  );
  const onConnected = vi.fn();
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "old-availability",
        onConnected,
      }),
    ),
  );
  await act(async () =>
    root.render(
      createElement(GoogleDriveConnection, {
        connectionId: "new-availability",
        onConnected,
      }),
    ),
  );
  await act(async () => fail(new Error("old availability failed")));
  expect(host.textContent).not.toContain("old availability failed");
  expect(host.textContent).toContain("Connect Google Drive");
});
