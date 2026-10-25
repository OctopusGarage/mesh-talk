// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  auth: { user: { id: "alice" }, generation: 1 },
  lock: { locked: false },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@/store/auth", () => ({ useAuth: { getState: () => mocks.auth } }));
vi.mock("@/store/appLock", () => ({
  useAppLock: { getState: () => mocks.lock },
}));
import { invokeSensitive } from "@/lib/api/sensitiveInvoke";
import { SensitiveVerificationDialog } from "./SensitiveVerificationDialog";

let root: Root;
let host: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.invoke.mockReset();
  mocks.auth = { user: { id: "alice" }, generation: 1 };
  mocks.lock.locked = false;
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(SensitiveVerificationDialog, {
        owner: "alice",
        generation: 1,
      }),
    ),
  );
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function request() {
  mocks.invoke.mockRejectedValueOnce({
    kind: "authentication-required",
    message: "Verify",
  });
  let pending!: Promise<unknown>;
  await act(async () => {
    pending = invokeSensitive("create_backup", {
      destination: "archive",
    }).catch((error) => error);
  });
  return { pending };
}
it("shows the requested action and cancels without resubmitting", async () => {
  const { pending } = await request();
  expect(document.body.textContent).toContain("Create an encrypted backup");
  const cancel = [...document.querySelectorAll("button")].find(
    (button) => button.textContent === "Cancel",
  )!;
  await act(async () => cancel.click());
  expect(await pending).toMatchObject({ kind: "authentication-canceled" });
  expect(mocks.invoke).toHaveBeenCalledTimes(1);
  expect(document.querySelector('input[type="password"]')).toBeNull();
});
it("submits the entered password once and removes it from the dialog", async () => {
  const { pending } = await request();
  mocks.invoke.mockResolvedValueOnce("created");
  const input = document.querySelector(
    'input[type="password"]',
  ) as HTMLInputElement;
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!;
    setter.call(input, "secret");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () =>
    document
      .querySelector("form")!
      .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
  );
  expect(await pending).toBe("created");
  expect(mocks.invoke).toHaveBeenLastCalledWith("create_backup", {
    destination: "archive",
    password: "secret",
  });
  expect(document.querySelector('input[type="password"]')).toBeNull();
});
it("cannot submit for a replacement session even before React rerenders", async () => {
  const { pending } = await request();
  mocks.auth.generation++;
  await act(async () =>
    document
      .querySelector("form")!
      .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
  );
  expect(await pending).toMatchObject({ kind: "authentication-canceled" });
  expect(mocks.invoke).toHaveBeenCalledTimes(1);
});
it("settles pending verification when the signed-in view unmounts", async () => {
  const { pending } = await request();
  await act(async () => root.render(null));
  expect(await pending).toMatchObject({ kind: "authentication-canceled" });
  expect(mocks.invoke).toHaveBeenCalledTimes(1);
});

it("rejects a lock and unlock transition even if the prompt host has not rerendered", async () => {
  const { pending } = await request();
  // App-lock synchronously increments this generation before clearing sensitive state.
  mocks.lock.locked = true;
  mocks.auth.generation++;
  mocks.lock.locked = false;
  await act(async () =>
    document
      .querySelector("form")!
      .dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })),
  );
  expect(await pending).toMatchObject({ kind: "authentication-canceled" });
  expect(mocks.invoke).toHaveBeenCalledTimes(1);
});
