// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";
import { DeviceEnrollmentDialog } from "./DeviceEnrollmentDialog";

const native = vi.hoisted(() => ({
  beginDeviceEnrollment: vi.fn(),
  probeDeviceEnrollment: vi.fn(),
  completeDeviceEnrollment: vi.fn(),
  cancelDeviceEnrollment: vi.fn(),
}));
vi.mock("@/lib/api/auth", () => ({ auth: native }));
const source = {
  id: "source-ticket",
  device_id: "device-fingerprint",
  account_id: "kept-account",
  nickname: "Kept nickname",
  addresses: ["192.0.2.4:47475"],
};
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  useAuth.setState({
    user: null,
    generation: 1,
    operation: 1,
    booting: false,
    loading: false,
    error: null,
  });
  native.beginDeviceEnrollment.mockReset().mockResolvedValue({ id: "attempt" });
  native.probeDeviceEnrollment.mockReset().mockResolvedValue([source]);
  native.completeDeviceEnrollment.mockReset().mockResolvedValue({
    success: true,
    user: {
      id: "local-device-user",
      username: "kept-login",
      display_name: "Kept nickname",
    },
  });
  native.cancelDeviceEnrollment.mockReset().mockResolvedValue(undefined);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
const dialog = () =>
  document.querySelector<HTMLElement>(
    '[data-testid="device-enrollment-dialog"]',
  );
async function open() {
  await act(async () => root.render(createElement(DeviceEnrollmentDialog)));
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="login-link-device"]')!
      .click(),
  );
}
async function input(selector: string, value: string) {
  const node = dialog()!.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function prepare() {
  await act(async () => {
    const select =
      dialog()!.querySelector<HTMLSelectElement>("#enrollment-source")!;
    select.value = source.id;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await input("#enrollment-code", "aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa");
  await act(async () =>
    dialog()!.querySelector<HTMLInputElement>("#enrollment-consent")!.click(),
  );
}
function submit() {
  return dialog()!.querySelector<HTMLButtonElement>(
    '[data-testid="enrollment-submit"]',
  )!;
}
async function close() {
  await act(async () =>
    dialog()!
      .querySelector<HTMLButtonElement>('[data-testid="enrollment-cancel"]')!
      .click(),
  );
}

it("links from the signed-out screen with explicit source consent and the normal authenticated session", async () => {
  await open();
  expect(native.beginDeviceEnrollment).toHaveBeenCalledOnce();
  expect(native.probeDeviceEnrollment).toHaveBeenCalledWith("attempt", []);
  expect(dialog()!.textContent).toContain("same login name and password");
  expect(dialog()!.querySelector('input[type="password"]')).toBeNull();
  expect(submit().disabled).toBe(true);
  await prepare();
  expect(dialog()!.textContent).toContain("Kept nickname");
  expect(dialog()!.textContent).toContain("kept-account");
  expect(submit().disabled).toBe(false);
  await act(async () =>
    dialog()!.querySelector<HTMLInputElement>("#enrollment-history")!.click(),
  );
  await act(async () => submit().click());
  expect(native.completeDeviceEnrollment).toHaveBeenCalledExactlyOnceWith(
    "attempt",
    source.id,
    "a".repeat(32),
    false,
  );
  expect(useAuth.getState().user?.username).toBe("kept-login");
});

it("cancels a late native enrollment ticket after its dialog has already closed", async () => {
  let finish!: (value: { id: string }) => void;
  native.beginDeviceEnrollment.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await open();
  await close();
  await act(async () => finish({ id: "late-attempt" }));
  expect(native.cancelDeviceEnrollment).toHaveBeenCalledExactlyOnceWith(
    "late-attempt",
  );
  expect(native.probeDeviceEnrollment).not.toHaveBeenCalled();
  expect(dialog()).toBeNull();
});

it("blocks close and mutable inputs while native enrollment publishes the account", async () => {
  let fail!: (reason: Error) => void;
  native.completeDeviceEnrollment.mockImplementationOnce(
    () =>
      new Promise((_, reject) => {
        fail = reject;
      }),
  );
  await open();
  await prepare();
  await act(async () => submit().click());
  expect(
    dialog()!.querySelector<HTMLButtonElement>(
      '[data-testid="enrollment-cancel"]',
    )!.disabled,
  ).toBe(true);
  expect(
    dialog()!.querySelector<HTMLInputElement>("#enrollment-code")!.disabled,
  ).toBe(true);
  await act(async () =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(dialog()).not.toBeNull();
  expect(native.cancelDeviceEnrollment).not.toHaveBeenCalled();
  await act(async () =>
    fail(new Error("Source connection interrupted; use a fresh code")),
  );
  expect(useAuth.getState().user).toBeNull();
  expect(dialog()!.querySelector('[role="alert"]')?.textContent).toContain(
    "fresh code",
  );
  await close();
  expect(native.cancelDeviceEnrollment).toHaveBeenCalledWith("attempt");
});

it("probes optional direct addresses and requires fresh consent after choosing a different source", async () => {
  native.probeDeviceEnrollment.mockResolvedValue([
    source,
    {
      ...source,
      id: "second-source",
      device_id: "second-device",
      account_id: "second-account",
      nickname: "Other account",
    },
  ]);
  await open();
  await input("#enrollment-targets", "192.0.2.4:47474, 192.0.2.5:47474");
  await act(async () =>
    dialog()!
      .querySelector<HTMLButtonElement>('[data-testid="enrollment-scan"]')!
      .click(),
  );
  expect(native.probeDeviceEnrollment).toHaveBeenLastCalledWith("attempt", [
    "192.0.2.4:47474",
    "192.0.2.5:47474",
  ]);
  await prepare();
  await act(async () => {
    const select =
      dialog()!.querySelector<HTMLSelectElement>("#enrollment-source")!;
    select.value = "second-source";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(
    dialog()!.querySelector<HTMLInputElement>("#enrollment-consent")!.checked,
  ).toBe(false);
  expect(submit().disabled).toBe(true);
  expect(native.completeDeviceEnrollment).not.toHaveBeenCalled();
});

it("waits for late ticket cancellation before starting a replacement enrollment", async () => {
  let finishBegin!: (value: { id: string }) => void;
  let finishCancel!: () => void;
  native.beginDeviceEnrollment
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishBegin = resolve;
        }),
    )
    .mockResolvedValue({ id: "new-attempt" });
  native.cancelDeviceEnrollment.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finishCancel = resolve;
      }),
  );
  await open();
  await close();
  await open();
  expect(native.beginDeviceEnrollment).toHaveBeenCalledTimes(1);
  await act(async () => finishBegin({ id: "old-attempt" }));
  expect(native.cancelDeviceEnrollment).toHaveBeenCalledExactlyOnceWith(
    "old-attempt",
  );
  expect(native.beginDeviceEnrollment).toHaveBeenCalledTimes(1);
  await act(async () => finishCancel());
  expect(native.beginDeviceEnrollment).toHaveBeenCalledTimes(2);
  expect(native.probeDeviceEnrollment).toHaveBeenCalledExactlyOnceWith(
    "new-attempt",
    [],
  );
});

it("ignores an old probe response after a replacement enrollment has found its source", async () => {
  let finish!: (value: (typeof source)[]) => void;
  native.probeDeviceEnrollment.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await open();
  await close();
  native.beginDeviceEnrollment.mockResolvedValue({ id: "new-attempt" });
  native.probeDeviceEnrollment.mockResolvedValue([
    { ...source, id: "new-source", nickname: "Current source" },
  ]);
  await open();
  await act(async () => finish([{ ...source, nickname: "Retired source" }]));
  expect(dialog()!.textContent).toContain("Current source");
  expect(dialog()!.textContent).not.toContain("Retired source");
});
