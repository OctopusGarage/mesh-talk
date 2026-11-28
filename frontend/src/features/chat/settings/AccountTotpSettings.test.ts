// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
const m = vi.hoisted(() => ({
  status: vi.fn(),
  begin: vi.fn(),
  confirm: vi.fn(),
  disable: vi.fn(),
  cancel: vi.fn(),
  retry: vi.fn(),
  current: true,
}));
vi.mock("@/lib/api/accountTotp", () => ({ accountTotp: m }));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "alice", current: () => m.current }),
}));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (s: unknown) => unknown) =>
    select({ user: { id: "alice" }, generation: 1 }),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: unknown) => unknown) =>
    select({ runEpoch: 1, identityEpoch: 1 }),
}));
import { AccountTotpSettings } from "./AccountTotpSettings";
let host: HTMLDivElement, root: Root;
async function click(text: string) {
  await act(async () => {
    Array.from(host.querySelectorAll("button"))
      .find((b) => b.textContent === text)!
      .click();
  });
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.clearAllMocks();
  m.current = true;
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function mount() {
  await act(async () => root.render(createElement(AccountTotpSettings)));
}
it("does not present unknown status as off or allow enrollment", async () => {
  m.status.mockReturnValue(new Promise(() => {}));
  await mount();
  expect(host.textContent).not.toContain("Set up");
  expect(host.textContent).not.toContain("Enabled locally");
});
it("shows real disabled status, local QR and recovery acknowledgement, and clears on cancel", async () => {
  m.status.mockResolvedValue({
    enabled: false,
    generation: 0,
    recovery_remaining: 0,
  });
  m.begin.mockResolvedValue({
    manual_seed: "SECRET",
    otpauth_uri: "otpauth://totp/test?secret=SECRET",
    recovery_codes: ["RECOVERY"],
  });
  await mount();
  await click("Set up");
  expect(host.querySelector("svg")).not.toBeNull();
  expect(host.textContent).toContain("RECOVERY");
  expect(
    Array.from(host.querySelectorAll("button")).find(
      (b) => b.textContent === "Confirm enrollment",
    )!.disabled,
  ).toBe(true);
  await click("Cancel");
  expect(host.textContent).not.toContain("SECRET");
  expect(m.cancel).toHaveBeenCalledWith("alice");
});
it("discards a stale owner completion", async () => {
  let resolve!: (value: unknown) => void;
  m.status.mockReturnValue(
    new Promise((r) => {
      resolve = r;
    }),
  );
  await mount();
  m.current = false;
  await act(async () => resolve({ enabled: true }));
  expect(host.textContent).not.toContain("Enabled locally");
});
it("keeps status failure unknown and offers retry", async () => {
  m.status.mockRejectedValue(new Error("private details"));
  await mount();
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
  expect(host.textContent).not.toContain("private details");
  expect(host.textContent).not.toContain("Set up");
});

async function prepareConfirmation() {
  m.status.mockResolvedValue({
    enabled: false,
    generation: 0,
    recovery_remaining: 0,
  });
  m.begin.mockResolvedValue({
    manual_seed: "SECRET",
    otpauth_uri: "otpauth://totp/test?secret=SECRET",
    recovery_codes: ["RECOVERY"],
  });
  await mount();
  await click("Set up");
  await act(async () => {
    (host.querySelector('input[type="checkbox"]') as HTMLInputElement).click();
    const input = host.querySelector(
      'input:not([type="checkbox"])',
    ) as HTMLInputElement;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "123456");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("retains enrollment after an invalid confirmation for retry", async () => {
  m.confirm.mockRejectedValue(new Error("Invalid code"));
  await prepareConfirmation();
  await click("Confirm enrollment");
  expect(m.confirm).toHaveBeenCalledWith("alice", "123456");
  expect(host.textContent).toContain("SECRET");
  expect(host.textContent).toContain("Confirm enrollment");
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
});
it("reports local enable separately from pending sync and explicitly retries publication", async () => {
  m.confirm.mockImplementation(async () => {
    m.status.mockResolvedValue({
      enabled: true,
      generation: 1,
      recovery_remaining: 8,
    });
    return { sync_pending: true };
  });
  m.retry.mockResolvedValue({ sync_pending: false });
  await prepareConfirmation();
  await click("Confirm enrollment");
  expect(host.textContent).toContain("Enabled locally");
  expect(host.textContent).toContain("synchronization is pending");
  expect(host.textContent).not.toContain("SECRET");
  await click("Retry");
  expect(m.retry).toHaveBeenCalledWith("alice");
  expect(host.textContent).not.toContain("synchronization is pending");
});

it("disables only with a supplied factor and reads the resulting policy", async () => {
  m.status.mockResolvedValue({
    enabled: true,
    generation: 1,
    recovery_remaining: 8,
  });
  m.disable.mockImplementation(async () => {
    m.status.mockResolvedValue({
      enabled: false,
      generation: 2,
      recovery_remaining: 0,
    });
    return { sync_pending: false };
  });
  await mount();
  expect(
    Array.from(host.querySelectorAll("button")).find(
      (b) => b.textContent === "Disable",
    )!.disabled,
  ).toBe(true);
  await act(async () => {
    const input = host.querySelector("input")!;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "RECOVERY");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await click("Disable");
  expect(m.disable).toHaveBeenCalledWith("alice", "RECOVERY");
  expect(host.textContent).toContain("Set up");
});
