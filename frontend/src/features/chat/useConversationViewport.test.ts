// @vitest-environment jsdom
import { act, createElement, useLayoutEffect } from "react";
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
  rows = messages,
  onViewed,
  reportInitialBottom = false,
}: {
  conversation: string;
  search?: Search;
  rows?: ChatMessage[];
  onViewed?: (key: string, ids: string[]) => void;
  reportInitialBottom?: boolean;
}) {
  viewport = useConversationViewport(
    conversation,
    rows,
    false,
    false,
    search,
    onViewed,
  );
  useLayoutEffect(() => {
    if (reportInitialBottom) viewport.atBottomStateChange(true);
  }, [conversation, reportInitialBottom]);
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
  viewport.stopRestoring();
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

  // A late virtual-list measurement must not replace the restored position.
  await act(async () => viewport.atBottomStateChange(true));
  expect(viewport.showJump).toBe(true);
  scroller.scrollTop = 700;
  await act(async () => {
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>);
  });
  expect(scroller.scrollTop).toBe(120);
  viewport.stopRestoring();
  scroller.scrollTop = 240;
  await act(async () => {
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>);
  });
  expect(scroller.scrollTop).toBe(240);
});

it("shows the jump button from the measured scroll position despite a stale bottom callback", async () => {
  await render("account:alice");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  viewport.stopRestoring();
  scroller.scrollTop = 0;
  await act(async () => {
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>);
  });
  expect(viewport.showJump).toBe(true);

  await act(async () => viewport.atBottomStateChange(true));
  expect(viewport.showJump).toBe(true);

  scroller.scrollTop = 700;
  await act(async () => {
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>);
  });
  expect(viewport.showJump).toBe(false);
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

  scrollTo.mockClear();
  viewport.jumpToLatest();
  await nextAnimationFrame();
  expect(scrollTo).toHaveBeenCalledTimes(2);
});

it.each([0, 1])(
  "keeps the visible message and offset when older rows are inserted at index %i",
  async (insertAt) => {
    await render("channel:group");
    const scroller = container.firstElementChild as HTMLElement;
    Object.defineProperties(scroller, {
      scrollHeight: { value: 1500, configurable: true },
      clientHeight: { value: 300, configurable: true },
    });
    scroller.getBoundingClientRect = () => ({ top: 0, bottom: 300 }) as DOMRect;
    const row = document.createElement("div");
    row.dataset.messageId = "target";
    scroller.append(row);
    let rowTop = -20;
    row.getBoundingClientRect = () =>
      ({ top: rowTop, bottom: rowTop + 100 }) as DOMRect;
    scroller.scrollTop = 120;
    viewport.stopRestoring();
    await act(async () =>
      viewport.onScroll({
        currentTarget: scroller,
      } as React.UIEvent<HTMLElement>),
    );
    rowTop = 80; // Prepend/middle insertion changes geometry, not message identity.
    await act(async () =>
      root.render(
        createElement(Harness, {
          conversation: "channel:group",
          rows: [
            ...messages.slice(0, insertAt),
            { ...messages[0], id: "late" },
            ...messages.slice(insertAt),
          ],
        }),
      ),
    );
    await nextAnimationFrame();
    expect(scroller.scrollTop).toBe(220);
  },
);

it("marks only actual intersecting rows viewed and does not read background content", async () => {
  const viewed = vi.fn();
  vi.spyOn(document, "hasFocus").mockReturnValue(false);
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  await act(async () =>
    root.render(
      createElement(Harness, {
        conversation: "channel:group",
        onViewed: viewed,
      }),
    ),
  );
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  scroller.getBoundingClientRect = () => ({ top: 100, bottom: 400 }) as DOMRect;
  for (const [id, top, bottom] of [
    ["above", 0, 100],
    ["visible", 150, 250],
    ["below", 400, 500],
  ] as const) {
    const row = document.createElement("div");
    row.dataset.messageId = id;
    row.getBoundingClientRect = () => ({ top, bottom }) as DOMRect;
    scroller.append(row);
  }
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  const separatorOnly = document.createElement("div");
  separatorOnly.dataset.messageId = "separator-only";
  separatorOnly.getBoundingClientRect = () =>
    ({ top: 350, bottom: 550 }) as DOMRect;
  const article = document.createElement("div");
  article.setAttribute("role", "article");
  article.getBoundingClientRect = () => ({ top: 450, bottom: 550 }) as DOMRect;
  separatorOnly.append(article);
  scroller.append(separatorOnly);
  expect(viewed).not.toHaveBeenCalled();
  vi.mocked(document.hasFocus).mockReturnValue(true);
  window.dispatchEvent(new Event("focus"));
  await nextAnimationFrame();
  expect(viewed).toHaveBeenLastCalledWith("channel:group", ["visible"]);
  vi.restoreAllMocks();
});

it("reports an unavailable unread target rather than pretending navigation succeeded", async () => {
  await render("channel:group");
  expect(viewport.jumpToMessage("outside-loaded-history")).toBe(false);
  const scrollToIndex = vi.fn();
  viewport.virtuosoRef.current = { scrollToIndex } as unknown as VirtuosoHandle;
  await act(async () => expect(viewport.jumpToMessage("target")).toBe(true));
  expect(scrollToIndex).toHaveBeenCalledWith({
    index: 1,
    align: "center",
    behavior: "auto",
  });
});

it("preserves the saved offset when a status strip resizes the viewport before history reload", async () => {
  await render("channel:group");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1500, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  let viewportTop = 0;
  scroller.getBoundingClientRect = () =>
    ({ top: viewportTop, bottom: 300 }) as DOMRect;
  const row = document.createElement("div");
  row.dataset.messageId = "target";
  row.getBoundingClientRect = () =>
    ({
      top: 100 - scroller.scrollTop,
      bottom: 200 - scroller.scrollTop,
    }) as DOMRect;
  scroller.append(row);
  scroller.scrollTop = 120;
  viewport.stopRestoring();
  viewport.stopRestoring();
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  // The browser keeps the row at the same screen position while a strip appears.
  // Its offset within the conversation must remain -20, rather than becoming -49.
  viewportTop = 29;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  expect(scroller.scrollTop).toBe(91);
  expect(row.getBoundingClientRect().top - viewportTop).toBe(-20);
});

it("keeps following the live edge when status height changes before an append", async () => {
  await render("channel:group");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  scroller.scrollTop = 700;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  Object.defineProperty(scroller, "clientHeight", {
    value: 271,
    configurable: true,
  });
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  expect(scroller.scrollTop).toBe(729);
  Object.defineProperty(scroller, "scrollHeight", {
    value: 1024,
    configurable: true,
  });
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  expect(scroller.scrollTop).toBe(753);
  expect(viewport.showJump).toBe(false);
});

it("keeps the initial list index fixed while the same conversation is mounted", async () => {
  await render("channel:group");
  expect(viewport.initialIndex).toBe(2);
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  viewport.stopRestoring();
  scroller.scrollTop = 120;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  await render("channel:group");
  expect(viewport.initialIndex).toBe(2);
});

it("honors the first user scroll even when it arrives after the interaction window", async () => {
  await render("channel:group");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  const now = vi.spyOn(performance, "now").mockReturnValue(1000);
  scroller.scrollTop = 700;
  viewport.stopRestoring();
  now.mockReturnValue(1300);
  scroller.scrollTop = 250;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  expect(scroller.scrollTop).toBe(250);
  expect(viewport.showJump).toBe(true);
  now.mockRestore();
});

it("does not consume scroll intent during an unrelated visibility measurement", async () => {
  await render("channel:group");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  scroller.getBoundingClientRect = () => ({ top: 0, bottom: 300 }) as DOMRect;
  const row = document.createElement("div");
  row.dataset.messageId = "target";
  row.getBoundingClientRect = () =>
    ({
      top: 150 - scroller.scrollTop,
      bottom: 450 - scroller.scrollTop,
    }) as DOMRect;
  scroller.append(row);
  const now = vi.spyOn(performance, "now").mockReturnValue(1000);
  viewport.stopRestoring();
  scroller.scrollTop = 100;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  now.mockReturnValue(1100);
  viewport.stopRestoring();
  window.dispatchEvent(new Event("focus"));
  await nextAnimationFrame();
  now.mockReturnValue(1400);
  scroller.scrollTop = 250;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  expect(scroller.scrollTop).toBe(250);
  now.mockRestore();
});

it("preserves a saved position when the remounted list reports bottom before measuring rows", async () => {
  await render("account:alice");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  viewport.stopRestoring();
  scroller.scrollTop = 120;
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  await render("account:bob");
  scroller.scrollTop = 0;
  Object.defineProperty(scroller, "scrollHeight", {
    value: 300,
    configurable: true,
  });
  await act(async () =>
    root.render(
      createElement(Harness, {
        conversation: "account:alice",
        reportInitialBottom: true,
      }),
    ),
  );
  Object.defineProperty(scroller, "scrollHeight", {
    value: 1000,
    configurable: true,
  });
  await nextAnimationFrame();
  expect(scroller.scrollTop).toBe(120);
  expect(viewport.showJump).toBe(true);
});

it("updates jump visibility when restored rows grow after the first measurement", async () => {
  await render("account:alice");
  const scroller = container.firstElementChild as HTMLElement;
  Object.defineProperties(scroller, {
    scrollHeight: { value: 1000, configurable: true },
    clientHeight: { value: 300, configurable: true },
  });
  viewport.stopRestoring();
  await act(async () =>
    viewport.onScroll({
      currentTarget: scroller,
    } as React.UIEvent<HTMLElement>),
  );
  await render("account:bob");
  Object.defineProperty(scroller, "scrollHeight", {
    value: 300,
    configurable: true,
  });
  await render("account:alice");
  await nextAnimationFrame();
  Object.defineProperty(scroller, "scrollHeight", {
    value: 1000,
    configurable: true,
  });
  await act(async () => viewport.atBottomStateChange(false));
  expect(scroller.scrollTop).toBe(0);
  expect(viewport.showJump).toBe(true);
});
