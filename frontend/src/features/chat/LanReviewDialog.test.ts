// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { FavoriteSource } from "@/lib/api/messageFavorites";
import "@/lib/i18n";
import { LanReviewDialog } from "./LanReviewDialog";

const { invoke, owner } = vi.hoisted(() => ({
  invoke: vi.fn(),
  owner: { current: true },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "alice", current: () => owner.current }),
}));

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const source: FavoriteSource = {
  kind: "account",
  target: "bob",
  message_ids: ["selected"],
};
const grantId = "fixture-lan-review-id";
const url = "http://192.168.0.10:40000/example";

let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  owner.current = true;
  invoke.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});

async function renderDialog(expiresAt?: number) {
  const onClose = vi.fn();
  await act(async () =>
    root.render(createElement(LanReviewDialog, { source, expiresAt, onClose })),
  );
  return onClose;
}

function button(label: string) {
  return Array.from(document.querySelectorAll("button")).find(
    (element) => element.textContent === label,
  )!;
}

it("requires a plaintext preview and confirmation before creating a link, then revokes it before closing", async () => {
  invoke
    .mockResolvedValueOnce("<b>exact text</b>")
    .mockResolvedValueOnce({ id: grantId, url })
    .mockRejectedValueOnce(new Error("temporary revoke failure"))
    .mockResolvedValueOnce(undefined);
  const close = await renderDialog();

  expect(invoke.mock.calls).toEqual([
    ["owner_preview_lan_review", { owner: "alice", source }],
  ]);
  expect(document.body.textContent).toContain("unencrypted HTTP");
  expect(document.body.textContent).toContain("forwarded to");
  expect(document.body.textContent).toContain("<b>exact text</b>");
  expect(document.querySelector("[role=dialog] b")).toBeNull();

  await act(async () => button("Create temporary link").click());
  expect(invoke).toHaveBeenLastCalledWith("owner_start_lan_review", {
    owner: "alice",
    source,
    preview: "<b>exact text</b>",
  });
  expect(document.querySelector("input")?.value).toBe(url);
  expect(document.body.textContent).not.toContain("<b>exact text</b>");

  await act(async () => button("Revoke link").click());
  expect(document.querySelector("input")?.value).toBe(url);
  expect(document.querySelector("[role=alert]")?.textContent).toContain(
    "Could not revoke",
  );
  expect(close).not.toHaveBeenCalled();

  await act(async () => button("Revoke link").click());
  expect(invoke).toHaveBeenLastCalledWith("owner_revoke_lan_review", {
    owner: "alice",
    grantId,
  });
  expect(close).toHaveBeenCalledOnce();
});

it("revokes a link that was issued after the dialog unmounted", async () => {
  let finish!: (value: { id: string; url: string }) => void;
  invoke
    .mockResolvedValueOnce("exact text")
    .mockReturnValueOnce(
      new Promise<{ id: string; url: string }>((resolve) => (finish = resolve)),
    );
  await renderDialog();
  await act(async () => button("Create temporary link").click());
  await act(async () => root.render(null));
  await act(async () => finish({ id: grantId, url }));
  expect(invoke).toHaveBeenLastCalledWith("owner_revoke_lan_review", {
    owner: "alice",
    grantId,
  });
});

it("does not reveal an in-flight preview after owner replacement", async () => {
  let finish!: (value: string) => void;
  invoke.mockReturnValueOnce(
    new Promise<string>((resolve) => (finish = resolve)),
  );
  await renderDialog();
  owner.current = false;
  await act(async () => finish("retired owner's text"));
  expect(document.body.textContent).not.toContain("retired owner's text");
  expect(button("Create temporary link").disabled).toBe(true);
});

it("withholds the local preview when the selected message expires", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(5_000);
  invoke.mockResolvedValueOnce("expiring text");
  await renderDialog(5_500);
  expect(document.body.textContent).toContain("expiring text");
  await act(async () => vi.advanceTimersByTime(500));
  expect(document.body.textContent).not.toContain("expiring text");
  expect(button("Create temporary link").disabled).toBe(true);
  expect(invoke).toHaveBeenCalledOnce();
});

it("reports preview failure and cancels without issuing a grant", async () => {
  invoke.mockRejectedValueOnce(new Error("preview unavailable"));
  const close = await renderDialog();
  expect(document.querySelector("[role=alert]")?.textContent).toBeTruthy();
  expect(button("Create temporary link").disabled).toBe(true);
  await act(async () => button("Cancel").click());
  expect(close).toHaveBeenCalledOnce();
  expect(invoke).toHaveBeenCalledOnce();
});
it("retries grant creation and handles clipboard failure then success", async () => {
  const writeText = vi
    .fn()
    .mockRejectedValueOnce(new Error("denied"))
    .mockResolvedValueOnce(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  invoke
    .mockResolvedValueOnce("selected text")
    .mockRejectedValueOnce(new Error("start failed"))
    .mockResolvedValueOnce({ id: grantId, url })
    .mockResolvedValue(undefined);
  await renderDialog();
  await act(async () => button("Create temporary link").click());
  expect(document.querySelector("[role=alert]")).not.toBeNull();
  await act(async () => button("Create temporary link").click());
  const input = document.querySelector("input")!;
  await act(async () => input.focus());
  expect(input.selectionEnd! - input.selectionStart!).toBe(url.length);
  const copy = () =>
    Array.from(document.querySelectorAll("button")).find((element) =>
      /copy/i.test(element.textContent ?? ""),
    )!;
  await act(async () => copy().click());
  expect(writeText).toHaveBeenCalledWith(url);
  expect(document.querySelector("[role=alert]")).not.toBeNull();
  await act(async () => copy().click());
  expect(document.querySelector("[role=alert]")).toBeNull();
  expect(document.body.textContent).toContain("Link copied");
});

it("closes a preview on Escape and revokes an active link before Escape closes it", async () => {
  invoke.mockResolvedValueOnce("preview");
  const close = await renderDialog();
  await act(async () =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(close).toHaveBeenCalledOnce();
  invoke
    .mockResolvedValueOnce({ id: grantId, url })
    .mockResolvedValue(undefined);
  await act(async () => button("Create temporary link").click());
  await act(async () =>
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    ),
  );
  expect(invoke).toHaveBeenLastCalledWith("owner_revoke_lan_review", {
    owner: "alice",
    grantId,
  });
  expect(close).toHaveBeenCalledTimes(2);
});

it("rejects create and copy actions after the owner changes", async () => {
  invoke
    .mockResolvedValueOnce("preview")
    .mockResolvedValueOnce({ id: grantId, url });
  await renderDialog();
  owner.current = false;
  await act(async () => button("Create temporary link").click());
  expect(invoke).toHaveBeenCalledOnce();
  owner.current = true;
  await act(async () => button("Create temporary link").click());
  const writeText = vi.fn();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  owner.current = false;
  await act(async () => button("Copy link").click());
  expect(writeText).not.toHaveBeenCalled();
});

it("discards preview failures after the owner changes", async () => {
  let reject!: (error: Error) => void;
  invoke.mockReturnValueOnce(
    new Promise<string>((_, fail) => {
      reject = fail;
    }),
  );
  await renderDialog();
  owner.current = false;
  await act(async () => reject(new Error("retired")));
  expect(document.querySelector("[role=alert]")).toBeNull();
});
it("does not close or publish request failures after retirement", async () => {
  let finish!: () => void;
  invoke
    .mockResolvedValueOnce("preview")
    .mockResolvedValueOnce({ id: grantId, url })
    .mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
    );
  const close = await renderDialog();
  await act(async () => button("Create temporary link").click());
  await act(async () => button("Revoke link").click());
  owner.current = false;
  await act(async () => finish());
  expect(close).not.toHaveBeenCalled();
});
it("does not expose a grant issued after owner retirement", async () => {
  let finish!: (value: { id: string; url: string }) => void;
  invoke.mockResolvedValueOnce("preview").mockReturnValueOnce(
    new Promise<{ id: string; url: string }>((resolve) => {
      finish = resolve;
    }),
  );
  await renderDialog();
  await act(async () => button("Create temporary link").click());
  owner.current = false;
  await act(async () => finish({ id: grantId, url }));
  expect(document.querySelector("input")).toBeNull();
  expect(invoke).toHaveBeenCalledTimes(2);
});
it("discards clipboard completion and errors after owner replacement", async () => {
  let finish!: () => void;
  let fail!: (error: Error) => void;
  const writeText = vi
    .fn()
    .mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
    )
    .mockReturnValueOnce(
      new Promise<void>((_, reject) => {
        fail = reject;
      }),
    );
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  invoke
    .mockResolvedValueOnce("preview")
    .mockResolvedValueOnce({ id: grantId, url });
  await renderDialog();
  await act(async () => button("Create temporary link").click());
  await act(async () => button("Copy link").click());
  owner.current = false;
  await act(async () => finish());
  expect(document.body.textContent).not.toContain("Link copied");
  owner.current = true;
  await act(async () => button("Copy link").click());
  owner.current = false;
  await act(async () => fail(new Error("late")));
  expect(document.querySelector("[role=alert]")).toBeNull();
});

it("discards start failures after owner replacement", async () => {
  let fail!: (error: Error) => void;
  invoke.mockResolvedValueOnce("preview").mockReturnValueOnce(
    new Promise<void>((_, reject) => {
      fail = reject;
    }),
  );
  await renderDialog();
  await act(async () => button("Create temporary link").click());
  owner.current = false;
  await act(async () => fail(new Error("retired")));
  expect(document.querySelector("[role=alert]")).toBeNull();
});
it("does not request preview under a retired lease", async () => {
  owner.current = false;
  await renderDialog();
  expect(invoke).not.toHaveBeenCalled();
});
