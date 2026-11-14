// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { useChat, type ChatMessage } from "@/store/chat";

const observed = vi.hoisted(() => ({ viewport: vi.fn() }));
vi.mock("react-virtuoso", () => ({
  Virtuoso: (props: { data: ChatMessage[] }) => {
    observed.viewport();
    return createElement(
      "div",
      { "data-testid": "message-viewport" },
      props.data.map((message) => message.text).join("|"),
    );
  },
}));
vi.mock("./Composer", () => ({ Composer: () => null }));
vi.mock("./ConversationHeaders", () => ({
  ChannelHeader: () => null,
  DmHeader: () => null,
  PersonalHeader: () => null,
  ConversationBio: () => null,
}));
vi.mock("@/lib/motion", async (original) => ({
  ...(await original<typeof import("@/lib/motion")>()),
  useMotionOK: () => false,
}));
vi.mock("./useNotificationPermissionReminder", () => ({
  useNotificationPermissionReminder: () => ({ visible: false }),
}));
vi.mock("./favorites/useFavoriteMembership", () => ({
  useFavoriteMembership: () => new Set(),
}));
vi.mock("./favorites/useMessageCollection", () => ({
  useMessageCollection: () => ({ selected: [], selecting: false }),
}));
vi.mock("./attachments/useAttachmentIntake", () => ({
  useAttachmentIntake: () => ({}),
}));
vi.mock("./useConversationViewport", async (original) => ({
  ...(await original<typeof import("./useConversationViewport")>()),
  useConversationViewport: () => ({}),
}));

import { ConversationView } from "./ConversationView";

afterEach(() => {
  vi.restoreAllMocks();
});

it("updates peer-check status without rerendering the message viewport, while new messages remain live", async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  const key = "account:me";
  const first: ChatMessage = {
    id: "first",
    fromMe: false,
    who: "Me",
    text: "Existing message",
    wallClock: Date.now(),
    replyTo: null,
  };
  useChat.setState({
    active: { kind: "account", id: "me", name: "Me" },
    myAccountId: "me",
    messages: { [key]: [first] },
    reception: { [key]: { phase: "idle", received: 0, checkedAt: 1000 } },
    ready: true,
    loading: false,
  });
  const host = document.createElement("div");
  const root = createRoot(host);
  try {
    await act(async () => root.render(createElement(ConversationView)));
    expect(
      host.querySelector('[data-testid="message-viewport"]')?.textContent,
    ).toBe(first.text);
    const renders = observed.viewport.mock.calls.length;
    const initialTitle = host
      .querySelector('[data-testid="peer-check-observation"]')
      ?.getAttribute("title");
    for (let index = 1; index <= 10; index++) {
      await act(async () =>
        useChat.setState({
          reception: {
            [key]: {
              phase: "idle",
              received: 0,
              checkedAt: 1000 + index * 60_000,
            },
          },
        }),
      );
    }
    expect(
      host
        .querySelector('[data-testid="peer-check-observation"]')
        ?.getAttribute("title"),
    ).not.toBe(initialTitle);
    expect(observed.viewport).toHaveBeenCalledTimes(renders);
    const currentTitle = host
      .querySelector('[data-testid="peer-check-observation"]')
      ?.getAttribute("title");
    await act(async () =>
      useChat.setState({
        reception: {
          ...useChat.getState().reception,
          "account:other": { phase: "waiting", received: 0 },
        },
      }),
    );
    expect(
      host
        .querySelector('[data-testid="peer-check-observation"]')
        ?.getAttribute("title"),
    ).toBe(currentTitle);
    expect(observed.viewport).toHaveBeenCalledTimes(renders);
    await act(async () =>
      useChat.setState({
        messages: {
          [key]: [first, { ...first, id: "second", text: "New message" }],
        },
      }),
    );
    expect(
      host.querySelector('[data-testid="message-viewport"]')?.textContent,
    ).toContain("New message");
    expect(observed.viewport.mock.calls.length).toBeGreaterThan(renders);
    await act(async () =>
      useChat.setState({
        active: { kind: "account", id: "other", name: "Other" },
        messages: {
          "account:other": [
            { ...first, id: "other", text: "Other conversation" },
          ],
        },
      }),
    );
    expect(
      host.querySelector('[data-testid="peer-check-observation"]'),
    ).toBeNull();
    expect(
      host.querySelector('[data-testid="message-viewport"]')?.textContent,
    ).toBe("Other conversation");
  } finally {
    await act(async () => root.unmount());
  }
});
