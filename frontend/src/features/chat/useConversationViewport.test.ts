// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { VirtuosoHandle } from "react-virtuoso";
import type { ChatMessage } from "@/store/chat";
import { useConversationViewport } from "./useConversationViewport";

const messages: ChatMessage[] = [
  {
    id: "first",
    fromMe: false,
    who: "Alice",
    text: "first",
    wallClock: 1,
    replyTo: null,
  },
  {
    id: "target",
    fromMe: false,
    who: "Alice",
    text: "target",
    wallClock: 2,
    replyTo: null,
  },
  {
    id: "last",
    fromMe: true,
    who: "Me",
    text: "last",
    wallClock: 3,
    replyTo: null,
  },
];

type Search = Parameters<typeof useConversationViewport>[4];
let viewport: ReturnType<typeof useConversationViewport>;
let root: Root;
let container: HTMLDivElement;
let nextFrame = 0;
let frames: Map<number, FrameRequestCallback>;

function Harness({
  conversation,
  search = null,
}: {
  conversation: string;
  search?: Search;
}) {
  viewport = useConversationViewport(
    conversation,
    messages,
    false,
    false,
    search,
  );
  return createElement("div", { ref: viewport.scrollerRef });
}

async function render(conversation: string, search: Search = null) {
  await act(async () => {
    root.render(createElement(Harness, { conversation, search }));
  });
}

async function nextAnimationFrame() {
  const ready = [...frames.values()];
  frames.clear();
  await act(async () => {
    for (const frame of ready) frame(0);
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  frames = new Map();
  nextFrame = 0;
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    const id = ++nextFrame;
    frames.set(id, callback);
    return id;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

it("restores the reading position after switching away and back", async () => {
  await render("account:alice");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  scroller.scrollTop = 120;
  await act(async () => {
    viewport.rangeChanged(1);
    viewport.atBottomStateChange(false);
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>);
  });
  expect(viewport.showJump).toBe(true);

  await render("account:bob");
  scroller.scrollTop = 0;
  await render("account:alice");
  expect(viewport.initialIndex).toBe(0);
  await nextAnimationFrame();
  expect(scroller.scrollTop).toBe(120);
  expect(viewport.showJump).toBe(true);
});

it("navigates to an exact search result and reports a later miss", async () => {
  await render("account:alice");
  const scrollToIndex = vi.fn();
  const scrollTo = vi.fn();
  viewport.virtuosoRef.current = {
    scrollToIndex,
    scrollTo,
  } as unknown as VirtuosoHandle;

  await render("account:alice", {
    key: "account:alice",
    request: 1,
    wallClock: 2,
    text: "target",
    fromMe: false,
  });
  await nextAnimationFrame();
  expect(scrollToIndex).toHaveBeenCalledWith({
    index: 1,
    align: "center",
    behavior: "auto",
  });
  expect(viewport.highlightedKey).toBe("target");
  expect(viewport.searchMiss).toBe(false);

  await render("account:alice", {
    key: "account:alice",
    request: 2,
    wallClock: 99,
    text: "missing",
    fromMe: false,
  });
  expect(viewport.searchMiss).toBe(true);
  expect(scrollToIndex).toHaveBeenCalledTimes(1);

  viewport.jumpToLatest();
  await nextAnimationFrame();
  expect(scrollTo).toHaveBeenCalledTimes(2);
});
