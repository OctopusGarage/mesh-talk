// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChannelInfo } from "@/lib/types";
const mocks = vi.hoisted(() => ({
  api: {
    channelMetadata: vi.fn(),
    setChannelAnnouncement: vi.fn(),
    setChannelCover: vi.fn(),
  },
  current: true,
  state: {
    myAccountId: "account",
    refreshChannels: vi.fn(),
    channels: [] as ChannelInfo[],
  },
}));
vi.mock("@/lib/api", () => ({ chat: mocks.api, auth: {} }));
vi.mock("@/store/chat", () => ({
  useChat: Object.assign(
    (select: (s: typeof mocks.state) => unknown) => select(mocks.state),
    { getState: () => mocks.state },
  ),
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => ({ owner: "owner", current: () => mocks.current }),
}));
import { useAuth } from "@/store/auth";
import { ChannelMetadataSettings } from "./ChannelMetadataSettings";
let host: HTMLDivElement;
let root: Root;
const empty = () => ({
  cover: null,
  announcement: null,
  announcement_history: [],
});
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  useAuth.setState({
    user: { id: "owner", username: "me", display_name: "Me" },
    generation: 1,
  });
  mocks.current = true;
  mocks.state.channels = [];
  mocks.api.channelMetadata.mockReset().mockResolvedValue(empty());
  mocks.api.setChannelAnnouncement.mockReset().mockResolvedValue({
    ...empty(),
    announcement: {
      content: "Welcome",
      revision: "new",
      author: "account",
      authored_at: 1,
    },
  });
  mocks.api.setChannelCover.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
const render = (channelId = "group", canManage = true) =>
  act(async () => {
    root.render(
      createElement(ChannelMetadataSettings, { channelId, canManage }),
    );
  });
const click = (name: string) =>
  act(async () => {
    const button = [...host.querySelectorAll("button")].find(
      (b) => b.textContent === name,
    );
    expect(button, name).toBeTruthy();
    button!.click();
  });
async function input(text: string) {
  await act(async () => {
    const field = host.querySelector("textarea")!;
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(field, text);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("keeps draft editing private until preview and explicit publication", async () => {
  await render();
  await click("Write announcement");
  await input("Welcome");
  expect(mocks.api.setChannelAnnouncement).not.toHaveBeenCalled();
  await click("Preview");
  expect(host.textContent).toContain("Welcome");
  await click("Publish");
  expect(mocks.api.setChannelAnnouncement).toHaveBeenCalledWith(
    "owner",
    "account",
    "group",
    "Welcome",
    null,
  );
  expect(host.textContent).toContain("Published by Me");
});
it("updates an open member view when a new summary version arrives", async () => {
  await render("group", false);
  const metadata = {
    ...empty(),
    announcement: {
      content: "New remote notice",
      revision: "remote",
      author: "another-account",
      authored_at: 1,
    },
  };
  mocks.api.channelMetadata.mockResolvedValue(metadata);
  mocks.state.channels = [
    {
      channel_id: "group",
      name: "Group",
      owner: "someone",
      member_count: 1,
      metadata,
    },
  ];
  await render("group", false);
  expect(host.textContent).toContain("New remote notice");
});
it("members can read but cannot publish announcements or a shared cover", async () => {
  await render("group", false);
  expect(host.querySelector("textarea")).toBeNull();
  expect(host.textContent).not.toContain("Write announcement");
  expect(host.textContent).not.toContain("Set group photo");
});
it("distinguishes pending encrypted group content from an empty announcement", async () => {
  mocks.api.channelMetadata.mockResolvedValue({
    ...empty(),
    cover_pending: true,
    announcement_pending: true,
  });
  await render();
  expect(host.textContent).toContain("Waiting for group information");
  expect(host.textContent).not.toContain("No group announcement");
  expect(
    [...host.querySelectorAll("button")].find(
      (button) => button.textContent === "Write announcement",
    )?.disabled,
  ).toBe(true);
  expect(
    [...host.querySelectorAll("button")].find(
      (button) => button.textContent === "Set group photo",
    )?.disabled,
  ).toBe(true);
});
it("a failed publication keeps the draft and offers refresh before retry", async () => {
  mocks.api.setChannelAnnouncement.mockRejectedValueOnce(
    new Error("revision conflict"),
  );
  await render();
  await click("Write announcement");
  await input("Welcome");
  await click("Preview");
  await click("Publish");
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
  expect(host.textContent).toContain("Welcome");
});
it("does not apply a completed save to a replacement group", async () => {
  let finish!: (value: unknown) => void;
  mocks.api.setChannelAnnouncement.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await render();
  await click("Write announcement");
  await input("Welcome");
  await click("Preview");
  await click("Publish");
  await render("other");
  await act(async () =>
    finish({
      ...empty(),
      announcement: { content: "Wrong group", revision: "old" },
    }),
  );
  expect(host.textContent).not.toContain("Wrong group");
});
