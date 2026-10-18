// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { Composer } from "./Composer";
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
let root: Root;
let host: HTMLDivElement;
let unmounted: boolean;
const send = vi.fn<(text: string) => boolean | Promise<boolean>>(() => true);
const validate = vi.fn<(text: string) => Promise<string | null>>();
function deferred<T = string | null>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  send.mockReset().mockReturnValue(true);
  validate.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  unmounted = false;
});
afterEach(async () => {
  if (!unmounted) await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render(key = "alice:peer", admissionPending = false) {
  await act(async () =>
    root.render(
      createElement(Composer, {
        key,
        onSend: send,
        validateText: validate,
        admissionPending,
        initialDraft: "Please send the document tomorrow.",
        placeholder: "Message",
        mentionNames: [],
      }),
    ),
  );
}
async function click() {
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
}
async function edit(text: string) {
  await act(async () => {
    const input = host.querySelector("textarea")!;
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(input, text);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("preserves rejected text and explains the language requirement", async () => {
  validate.mockResolvedValue("Use at least 80% Japanese.");
  await render();
  await click();
  expect(send).not.toHaveBeenCalled();
  expect(host.querySelector("textarea")!.value).toBe(
    "Please send the document tomorrow.",
  );
  expect(host.querySelector('[role="alert"]')?.textContent).toBe(
    "Use at least 80% Japanese.",
  );
});
it("validates before sending and clearing a draft", async () => {
  const job = deferred();
  validate.mockReturnValue(job.promise);
  await render();
  await click();
  expect(validate).toHaveBeenCalledExactlyOnceWith(
    "Please send the document tomorrow.",
  );
  expect(send).not.toHaveBeenCalled();
  expect(host.querySelector('[role="status"]')?.textContent).toBe(
    "Checking message language…",
  );
  await act(async () => job.resolve(null));
  expect(send).toHaveBeenCalledExactlyOnceWith(
    "Please send the document tomorrow.",
  );
  expect(host.querySelector("textarea")!.value).toBe("");
});
it("keeps the draft when local validation fails", async () => {
  validate.mockRejectedValue(new Error("session ended"));
  await render();
  await click();
  expect(send).not.toHaveBeenCalled();
  expect(host.querySelector("textarea")!.value).not.toBe("");
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
});
it("suppresses duplicate sends while validation is pending", async () => {
  const job = deferred();
  validate.mockReturnValue(job.promise);
  await render();
  await click();
  await click();
  expect(validate).toHaveBeenCalledTimes(1);
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .disabled,
  ).toBe(true);
  await act(async () => job.resolve(null));
  expect(send).toHaveBeenCalledTimes(1);
});
it("does not send a draft edited while validation is pending", async () => {
  const job = deferred();
  validate.mockReturnValue(job.promise);
  await render();
  await click();
  await edit("A different message.");
  await act(async () => job.resolve(null));
  expect(send).not.toHaveBeenCalled();
  expect(host.querySelector("textarea")!.value).toBe("A different message.");
});
it.each(["bob:peer", "alice:another-peer"])(
  "ignores a stale result after switching to %s",
  async (key) => {
    const job = deferred();
    validate.mockReturnValue(job.promise);
    await render();
    await click();
    await render(key);
    await act(async () => job.resolve(null));
    expect(send).not.toHaveBeenCalled();
    expect(host.querySelector("textarea")!.value).not.toBe("");
  },
);
it("ignores a successful validation after unmount", async () => {
  const job = deferred();
  validate.mockReturnValue(job.promise);
  await render();
  await click();
  await act(async () => root.unmount());
  unmounted = true;
  await act(async () => job.resolve(null));
  expect(send).not.toHaveBeenCalled();
});

it("retains the draft when preflight passes but final durable admission rejects", async () => {
  const admission = deferred<boolean>();
  validate.mockResolvedValue(null);
  send.mockReturnValue(admission.promise);
  await render();
  await click();
  expect(send).toHaveBeenCalledTimes(1);
  expect(host.querySelector("textarea")!.value).toBe(
    "Please send the document tomorrow.",
  );
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .disabled,
  ).toBe(true);
  await act(async () => admission.resolve(false));
  expect(host.querySelector("textarea")!.value).toBe(
    "Please send the document tomorrow.",
  );
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .disabled,
  ).toBe(false);
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
  expect(send).toHaveBeenCalledTimes(1);
  expect(validate).toHaveBeenCalledTimes(1);
});
it("clears only after durable admission succeeds", async () => {
  const admission = deferred<boolean>();
  validate.mockResolvedValue(null);
  send.mockReturnValue(admission.promise);
  await render();
  await click();
  await click();
  expect(host.querySelector("textarea")!.value).not.toBe("");
  await act(async () => admission.resolve(true));
  expect(host.querySelector("textarea")!.value).toBe("");
  expect(send).toHaveBeenCalledTimes(1);
});
it("preserves edited drafts after an earlier admission succeeds", async () => {
  const admission = deferred<boolean>();
  validate.mockResolvedValue(null);
  send.mockReturnValue(admission.promise);
  await render();
  await click();
  await edit("Keep this new draft.");
  await act(async () => admission.resolve(true));
  expect(host.querySelector("textarea")!.value).toBe("Keep this new draft.");
});
it("keeps a draft when durable admission throws", async () => {
  validate.mockResolvedValue(null);
  send.mockRejectedValueOnce(new Error("policy changed"));
  await render();
  await click();
  expect(host.querySelector("textarea")!.value).not.toBe("");
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
});

it.each(["bob:peer", "alice:another-peer"])(
  "does not clear a replacement draft when durable admission resolves for %s",
  async (key) => {
    const admission = deferred<boolean>();
    validate.mockResolvedValue(null);
    send.mockReturnValue(admission.promise);
    await render();
    await click();
    await render(key);
    await act(async () => admission.resolve(true));
    expect(host.querySelector("textarea")!.value).toBe(
      "Please send the document tomorrow.",
    );
    expect(send).toHaveBeenCalledTimes(1);
  },
);

it("blocks re-admission when a remounted composer inherits a pending conversation send", async () => {
  validate.mockResolvedValue(null);
  await render("alice:peer", true);
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .disabled,
  ).toBe(true);
  await click();
  expect(send).not.toHaveBeenCalled();
  expect(validate).not.toHaveBeenCalled();
  expect(host.querySelector("textarea")!.value).toBe(
    "Please send the document tomorrow.",
  );
});
it("does not apply an external clear after its draft guard becomes stale", async () => {
  await act(async () =>
    root.render(
      createElement(Composer, {
        onSend: send,
        initialDraft: "Replacement draft",
        placeholder: "Message",
        mentionNames: [],
        prefill: { text: "", n: 1, applyIf: () => false },
      }),
    ),
  );
  expect(host.querySelector("textarea")!.value).toBe("Replacement draft");
});
