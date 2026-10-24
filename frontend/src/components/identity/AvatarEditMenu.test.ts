// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
const mocks = vi.hoisted(() => ({
  save: vi.fn(),
  pick: vi.fn(),
  current: true,
}));
vi.mock("@/store/avatars", () => ({
  useAvatar: () => "saved-photo",
  useAvatars: (select: (state: { setAvatar: typeof mocks.save }) => unknown) =>
    select({ setAvatar: mocks.save }),
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ current: () => mocks.current }),
}));
vi.mock("@/lib/avatarImage", () => ({ pickImageFile: mocks.pick }));
vi.mock("@/features/chat/AvatarCropDialog", () => ({
  AvatarCropDialog: ({
    file,
    onConfirm,
  }: {
    file: File | null;
    onConfirm: (value: string) => void;
  }) =>
    file
      ? createElement(
          "button",
          { onClick: () => onConfirm("chosen-photo") },
          "Confirm crop",
        )
      : null,
}));
import { AvatarEditMenu } from "./AvatarEditMenu";
let root: Root;
let host: HTMLDivElement;
const render = async (id = "contact") =>
  act(async () => {
    root.render(
      createElement(AvatarEditMenu, {
        id,
        ariaLabel: "Edit photo",
        children: createElement("span", null, "Photo"),
      }),
    );
  });
const button = (text: string) =>
  [...document.querySelectorAll<HTMLButtonElement>("button")].find(
    (element) => element.textContent === text,
  )!;
const open = () =>
  act(async () => {
    document
      .querySelector<HTMLButtonElement>('[aria-label="Edit photo"]')!
      .click();
  });
beforeEach(async () => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  mocks.current = true;
  mocks.save.mockReset().mockResolvedValue(undefined);
  mocks.pick.mockReset().mockResolvedValue(null);
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
it("shows a failed removal and retries the same choice after canonical reconciliation", async () => {
  mocks.save.mockRejectedValueOnce(new Error("CAS conflict"));
  await render();
  await open();
  await act(async () => button("Remove photo").click());
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(
    "Could not save the photo",
  );
  await act(async () => button("Retry").click());
  expect(mocks.save.mock.calls).toEqual([
    ["contact", null],
    ["contact", null],
  ]);
  expect(document.querySelector('[role="alert"]')).toBeNull();
});
it("shows a failed cropped photo and retries its original bytes", async () => {
  mocks.pick.mockResolvedValue(
    new File(["image"], "photo.png", { type: "image/png" }),
  );
  mocks.save.mockRejectedValueOnce(new Error("storage error"));
  await render();
  await open();
  await act(async () => button("Change photo…").click());
  await act(async () => button("Confirm crop").click());
  expect(document.querySelector('[role="alert"]')).not.toBeNull();
  await act(async () => button("Retry").click());
  expect(mocks.save.mock.calls).toEqual([
    ["contact", "chosen-photo"],
    ["contact", "chosen-photo"],
  ]);
});
it("does not reopen a replacement contact's picker for an earlier failure", async () => {
  let fail!: (error: Error) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise((_resolve, reject) => {
        fail = reject;
      }),
  );
  await render();
  await open();
  await act(async () => button("Remove photo").click());
  await render("replacement");
  await act(async () => fail(new Error("old failure")));
  expect(document.querySelector('[role="alert"]')).toBeNull();
});
it("does not expose a retired session's failure", async () => {
  let fail!: (error: Error) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise((_resolve, reject) => {
        fail = reject;
      }),
  );
  await render();
  await open();
  await act(async () => button("Remove photo").click());
  mocks.current = false;
  await act(async () => fail(new Error("old failure")));
  expect(document.querySelector('[role="alert"]')).toBeNull();
});

it("keeps a newer successful choice visible when an earlier action fails", async () => {
  let fail!: (error: Error) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise((_resolve, reject) => {
        fail = reject;
      }),
  );
  mocks.pick.mockResolvedValue(
    new File(["image"], "new.png", { type: "image/png" }),
  );
  await render();
  await open();
  await act(async () => button("Remove photo").click());
  await open();
  await act(async () => button("Change photo…").click());
  await act(async () => button("Confirm crop").click());
  await act(async () => fail(new Error("superseded failure")));
  expect(document.querySelector('[role="alert"]')).toBeNull();
  expect(mocks.save.mock.calls).toEqual([
    ["contact", null],
    ["contact", "chosen-photo"],
  ]);
});
