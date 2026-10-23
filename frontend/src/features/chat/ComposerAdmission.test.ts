// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { Composer } from "./Composer";
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
let root: Root;
let host: HTMLDivElement;
const send = vi.fn<(text: string) => boolean | Promise<boolean>>(() => true);
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  send.mockReset().mockReturnValue(true);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render(key = "alice:peer", admissionPending = false) {
  await act(async () =>
    root.render(
      createElement(Composer, {
        key,
        onSend: send,
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
it("retains the draft when durable admission rejects", async () => {
  const admission = deferred<boolean>();
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
});
it("clears only after durable admission succeeds", async () => {
  const admission = deferred<boolean>();
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
  send.mockReturnValue(admission.promise);
  await render();
  await click();
  await edit("Keep this new draft.");
  await act(async () => admission.resolve(true));
  expect(host.querySelector("textarea")!.value).toBe("Keep this new draft.");
});
it("keeps a draft when durable admission throws", async () => {
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
  await render("alice:peer", true);
  expect(
    host.querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .disabled,
  ).toBe(true);
  await click();
  expect(send).not.toHaveBeenCalled();
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

it.each([
  { isComposing: true, keyCode: 13 },
  { isComposing: false, keyCode: 229 },
])(
  "retains IME-confirmed text for $isComposing/$keyCode until a separate Enter",
  async (ime) => {
    await render();
    await edit("你好");
    const input = host.querySelector("textarea")!;
    await act(async () => {
      input.dispatchEvent(
        new CompositionEvent("compositionend", { bubbles: true, data: "你好" }),
      );
    });
    const confirm = new KeyboardEvent("keydown", {
      key: "Enter",
      code: "Enter",
      bubbles: true,
      cancelable: true,
      ...ime,
    });
    await act(async () => {
      input.dispatchEvent(confirm);
    });
    expect(send).not.toHaveBeenCalled();
    expect(input.value).toBe("你好");
    expect(confirm.defaultPrevented).toBe(false);

    await act(async () => {
      input.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          code: "Enter",
          keyCode: 13,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(send).toHaveBeenCalledExactlyOnceWith("你好");
    expect(input.value).toBe("");
  },
);

it("retains protection on rejection and resets it after acceptance", async () => {
  await render();
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('[data-testid="composer-protection"]')!
      .click(),
  );
  await act(async () =>
    document
      .querySelector<HTMLButtonElement>(
        '[data-testid="composer-protection-authenticate"]',
      )!
      .click(),
  );
  const selected = () =>
    host.textContent?.includes("Text & images · hides after 30 seconds");
  send.mockReturnValueOnce(false);
  await click();
  expect(send).toHaveBeenLastCalledWith(
    "Please send the document tomorrow.",
    "authenticate",
  );
  expect(selected()).toBe(true);
  await click();
  expect(selected()).toBe(false);
});

it.each([true, false])(
  "synchronizes accepted clear after remount only while its owning revision is current (%s)",
  async (current) => {
    await act(async () =>
      root.render(
        createElement(Composer, {
          onSend: send,
          initialDraft: "Accepted original",
          initialProtection: "click",
          placeholder: "Message",
          mentionNames: [],
        }),
      ),
    );
    if (!current) await edit("Replacement draft");
    await act(async () =>
      root.render(
        createElement(Composer, {
          onSend: send,
          initialDraft: "",
          initialProtection: undefined,
          acceptedClear: { n: 1, applyIf: () => current },
          placeholder: "Message",
          mentionNames: [],
        }),
      ),
    );
    expect(host.querySelector("textarea")!.value).toBe(
      current ? "" : "Replacement draft",
    );
    expect(
      host.textContent?.includes("Text & images · hides after 30 seconds"),
    ).toBe(!current);
  },
);
