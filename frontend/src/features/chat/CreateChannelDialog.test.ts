// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import "@/lib/i18n";

const state = vi.hoisted(() => ({
  myAccountId: "duncan",
  peers: [
    {
      user_id: "device-b",
      account_id: "einstein",
      name: "Old name",
      post_office: false,
    },
    {
      user_id: "device-a",
      account_id: "einstein",
      name: "Old name",
      post_office: false,
    },
    {
      user_id: "own-sibling",
      account_id: "duncan",
      name: "Duncan",
      post_office: false,
    },
    {
      user_id: "relay",
      account_id: "relay-account",
      name: "Post office",
      post_office: true,
    },
  ],
  accounts: [{ account_id: "einstein", names: ["爱咪斯坦"], device_count: 2 }],
  favorites: {},
  createChannel: vi.fn(),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: typeof state) => unknown) => select(state),
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: (select: (s: unknown) => unknown) =>
    select({ loaded: true, contacts: {} }),
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrapper = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return Object.fromEntries(
    [
      "Dialog",
      "DialogContent",
      "DialogHeader",
      "DialogTitle",
      "DialogDescription",
      "DialogTrigger",
    ].map((key) => [key, Wrapper]),
  );
});
vi.mock("@/components/identity", () => ({
  IdentityCrest: ({ id, name }: { id: string; name: string }) =>
    createElement("span", { "data-identity": id }, name),
}));

import { CreateChannelDialog } from "./CreateChannelDialog";

it("invites an account once, uses its nickname, and excludes our own linked devices", () => {
  const html = renderToStaticMarkup(createElement(CreateChannelDialog));
  expect(html.match(/data-identity=/g)).toHaveLength(1);
  expect(html).toContain('data-identity="einstein"');
  expect(html).toContain("爱咪斯坦");
  expect(html).not.toContain("own-sibling");
  expect(html).not.toContain("Each device must accept");
});

it("keeps the selected account when its representative device changes and submits account IDs", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const peers = state.peers;
  try {
    state.createChannel.mockResolvedValue(true);
    await act(async () => root.render(createElement(CreateChannelDialog)));
    await act(async () => {
      host
        .querySelector<HTMLButtonElement>('button[aria-pressed="false"]')!
        .click();
      const input = host.querySelector("input")!;
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, "Team");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    state.peers = peers.filter((peer) => peer.user_id !== "device-a");
    await act(async () => root.render(createElement(CreateChannelDialog)));
    expect(host.querySelectorAll('button[aria-pressed="true"]')).toHaveLength(
      1,
    );
    await act(async () => {
      [...host.querySelectorAll("button")]
        .find((button) => button.textContent === "Create")!
        .click();
    });
    expect(state.createChannel).toHaveBeenCalledWith("Team", ["einstein"]);
  } finally {
    state.peers = peers;
    await act(async () => root.unmount());
    host.remove();
    state.createChannel.mockReset();
  }
});

it("does not merge different accounts just because their nicknames match", () => {
  const peers = state.peers;
  try {
    state.peers = [
      ...peers,
      {
        user_id: "other",
        account_id: "other-account",
        name: "爱咪斯坦",
        post_office: false,
      },
    ];
    const html = renderToStaticMarkup(createElement(CreateChannelDialog));
    expect(html.match(/data-identity=/g)).toHaveLength(2);
    expect(html).toContain('data-identity="other-account"');
  } finally {
    state.peers = peers;
  }
});
