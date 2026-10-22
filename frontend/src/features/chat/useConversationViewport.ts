import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { VirtuosoHandle } from "react-virtuoso";
import type { ChatMessage, SearchTarget } from "@/store/chat";
import { jumpToLatest } from "./jumpToLatest";

export const messageRowKey = (message: ChatMessage, index: number) =>
  message.clientId ?? message.id ?? `pending-${index}`;

type Target = (SearchTarget & { key: string; request: number }) | null;

const isAtBottom = (scroller: HTMLElement) =>
  scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop <= 48;

/** Movement and restoration for the rendered virtual list, scoped by conversation. */
export function useConversationViewport(
  key: string,
  messages: ChatMessage[],
  loading: boolean,
  historyError: boolean,
  searchTarget: Target,
  onViewed?: (key: string, ids: string[]) => void,
) {
  const [showJump, setShowJump] = useState(false);
  const [highlightedKey, setHighlightedKey] = useState<string | null>(null);
  const [searchMiss, setSearchMiss] = useState(false);
  const exactSearch = searchTarget?.key === key && "reference" in searchTarget;
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const scrollerRef = useRef<HTMLElement | null>(null);
  const scrollPositions = useRef(new Map<string, number>());
  const exactScrollPositions = useRef(new Map<string, number>());
  const restoringScroll = useRef<string | null>(null);
  const lastVisibleRows = useRef(new Map<string, number>());
  const atBottom = useRef(new Map<string, boolean>());
  const anchors = useRef(
    new Map<
      string,
      {
        id: string;
        offset: number;
        viewportTop: number;
        viewportHeight: number;
      }
    >(),
  );
  const userInteractionUntil = useRef(0);
  const pendingUserAnchor = useRef(false);
  const pendingScrollIntent = useRef(false);
  const initialList = useRef<{
    key: string;
    index: number;
    searchRequest: number | null;
  } | null>(null);
  const renderedConversation = useRef(key);
  if (renderedConversation.current !== key) {
    renderedConversation.current = key;
    // Child layout measurements precede parent effects. Protect the saved
    // position before the remounted virtual list can report a temporary bottom.
    restoringScroll.current = exactScrollPositions.current.has(key)
      ? key
      : null;
  }
  const pinLiveEdge = useCallback(() => {
    const scroller = scrollerRef.current;
    if (
      !scroller ||
      exactSearch ||
      anchors.current.has(key) ||
      pendingUserAnchor.current ||
      pendingScrollIntent.current ||
      atBottom.current.get(key) === false ||
      restoringScroll.current === key ||
      performance.now() < userInteractionUntil.current
    )
      return;
    const top = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    // Keep Virtuoso's measurement state in sync with the requested movement.
    // Direct writes can fight its correction after an oversized row is measured.
    if (virtuosoRef.current)
      virtuosoRef.current.scrollTo({
        top: Number.MAX_SAFE_INTEGER,
        behavior: "auto",
      });
    else scroller.scrollTop = top;
  }, [key, exactSearch]);
  const restoreVisibleAnchor = useCallback(() => {
    const scroller = scrollerRef.current;
    const anchor = anchors.current.get(key);
    if (!scroller || !anchor) return;
    const bounds = scroller.getBoundingClientRect();
    const row = [
      ...scroller.querySelectorAll<HTMLElement>("[data-message-id]"),
    ].find((element) => element.dataset.messageId === anchor.id);
    if (!row) return;
    const delta = row.getBoundingClientRect().top - bounds.top - anchor.offset;
    if (Math.abs(delta) > 0.5) scroller.scrollTop += delta;
    anchor.viewportTop = bounds.top;
    anchor.viewportHeight = bounds.bottom - bounds.top;
    exactScrollPositions.current.set(key, scroller.scrollTop);
  }, [key]);
  const viewedRef = useRef(onViewed);
  viewedRef.current = onViewed;
  const measureRows = useCallback(
    (captureAnchor = true) => {
      const scroller = scrollerRef.current;
      if (!scroller) return;
      const bounds = scroller.getBoundingClientRect();
      const rows = [
        ...scroller.querySelectorAll<HTMLElement>("[data-message-id]"),
      ];
      const visible = rows.filter((row) => {
        const rect = row.getBoundingClientRect();
        return rect.bottom > bounds.top && rect.top < bounds.bottom;
      });
      if (captureAnchor && visible[0] && !isAtBottom(scroller)) {
        pendingUserAnchor.current = false;
        anchors.current.set(key, {
          id: visible[0].dataset.messageId!,
          offset: visible[0].getBoundingClientRect().top - bounds.top,
          viewportTop: bounds.top,
          viewportHeight: bounds.bottom - bounds.top,
        });
        if (anchors.current.size > 24)
          anchors.current.delete(anchors.current.keys().next().value!);
      }
      if (
        !loading &&
        !historyError &&
        document.visibilityState === "visible" &&
        document.hasFocus()
      )
        viewedRef.current?.(
          key,
          visible
            .filter((row) => {
              // Date separators belong to the anchor geometry, but seeing only a
              // separator is not evidence that the message itself was displayed.
              const article =
                row.querySelector<HTMLElement>('[role="article"]') ?? row;
              const rect = article.getBoundingClientRect();
              return rect.bottom > bounds.top && rect.top < bounds.bottom;
            })
            .map((row) => row.dataset.messageId!),
        );
    },
    [key, loading, historyError],
  );

  // Restore by identity after any insertion, including rows inserted in the middle.
  // DOM geometry includes variable-height messages and date separators.
  useLayoutEffect(() => {
    const anchor = anchors.current.get(key);
    if (
      !anchor ||
      loading ||
      historyError ||
      (searchTarget?.key === key &&
        handledSearch.current !== searchTarget.request)
    )
      return;
    const scroller = scrollerRef.current;
    if (!scroller) return;
    let frame = 0;
    let attempts = 0;
    const restoreAnchor = () => {
      if (anchors.current.get(key) !== anchor) return;
      const row = [
        ...scroller.querySelectorAll<HTMLElement>("[data-message-id]"),
      ].find((element) => element.dataset.messageId === anchor.id);
      if (row) {
        const delta =
          row.getBoundingClientRect().top -
          scroller.getBoundingClientRect().top -
          anchor.offset;
        scroller.scrollTop += delta;
        exactScrollPositions.current.set(key, scroller.scrollTop);
      } else if (attempts++ < 12) {
        const index = messages.findIndex(
          (m, i) => messageRowKey(m, i) === anchor.id,
        );
        if (index >= 0) {
          if (attempts === 1)
            virtuosoRef.current?.scrollToIndex({
              index,
              align: "start",
              offset: -anchor.offset,
              behavior: "auto",
            });
          frame = requestAnimationFrame(restoreAnchor);
        }
      }
    };
    restoreAnchor();
    return () => cancelAnimationFrame(frame);
  }, [key, messages, loading, historyError, searchTarget]);

  useEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    let frame = 0;
    const check = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        pinLiveEdge();
        measureRows(pendingUserAnchor.current && !pendingScrollIntent.current);
        // A deferred virtualizer correction can leave the measured viewport
        // above the live edge without a scroll event. Keep navigation available
        // while retaining the existing live-edge intent and reader anchor.
        setShowJump(!isAtBottom(scroller));
      });
    };
    const resize =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(() => {
            restoreVisibleAnchor();
            check();
          });
    resize?.observe(scroller);
    const observer = new MutationObserver(check);
    observer.observe(scroller, { childList: true, subtree: true });
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    check();
    return () => {
      observer.disconnect();
      resize?.disconnect();
      cancelAnimationFrame(frame);
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
    };
  }, [
    key,
    messages,
    loading,
    historyError,
    measureRows,
    restoreVisibleAnchor,
    pinLiveEdge,
  ]);

  const handledSearch = useRef<number | null>(null);

  useEffect(() => {
    pendingUserAnchor.current = false;
    pendingScrollIntent.current = false;
    userInteractionUntil.current = 0;
    setShowJump(false);
    setSearchMiss(false);
    setHighlightedKey(null);
  }, [key]);

  useEffect(() => {
    if (!key || !exactScrollPositions.current.has(key)) return;
    restoringScroll.current = key;
    return () => {
      if (restoringScroll.current === key) restoringScroll.current = null;
    };
  }, [key]);

  useEffect(() => {
    if (restoringScroll.current !== key || loading || messages.length === 0)
      return;
    let top = exactScrollPositions.current.get(key);
    if (top === undefined) return;
    let frame = 0;
    let attempts = 0;
    const restore = () => {
      const scroller = scrollerRef.current;
      const maxTop = scroller
        ? scroller.scrollHeight - scroller.clientHeight
        : 0;
      if ((!scroller || maxTop < top!) && attempts++ < 30) {
        frame = requestAnimationFrame(restore);
        return;
      }
      if (scroller) {
        const anchor = anchors.current.get(key);
        const row =
          anchor &&
          [...scroller.querySelectorAll<HTMLElement>("[data-message-id]")].find(
            (element) => element.dataset.messageId === anchor.id,
          );
        if (anchor && row) {
          top =
            scroller.scrollTop +
            row.getBoundingClientRect().top -
            scroller.getBoundingClientRect().top -
            anchor.offset;
          exactScrollPositions.current.set(key, top);
        }
        scroller.scrollTop = Math.min(top!, Math.max(0, maxTop));
        const bottom = maxTop - scroller.scrollTop <= 48;
        atBottom.current.set(key, bottom);
        setShowJump(!bottom);
      }
      // Virtuoso may adjust its scroll position again after measuring rows.
      // Keep the saved offset until the reader starts a new interaction.
    };
    frame = requestAnimationFrame(restore);
    return () => cancelAnimationFrame(frame);
  }, [key, loading, messages.length]);

  useEffect(() => {
    if (!searchTarget || searchTarget.key !== key || loading || historyError)
      return;
    const index = messages.findIndex((message) =>
      "reference" in searchTarget
        ? searchTarget.reference.message_id !== null &&
          message.id === searchTarget.reference.message_id
        : message.wallClock === searchTarget.wallClock &&
          message.text === searchTarget.text &&
          message.fromMe === searchTarget.fromMe,
    );
    if (index < 0) {
      handledSearch.current = searchTarget.request;
      setSearchMiss(true);
      setHighlightedKey(null);
      return;
    }
    setSearchMiss(false);
    if (handledSearch.current === searchTarget.request) return;
    setHighlightedKey(messageRowKey(messages[index], index));
    const frame = requestAnimationFrame(() => {
      if (restoringScroll.current === key) restoringScroll.current = null;
      pendingUserAnchor.current = false;
      anchors.current.delete(key);
      atBottom.current.set(key, false);
      virtuosoRef.current?.scrollToIndex({
        index,
        align: "center",
        behavior: "auto",
      });
      handledSearch.current = searchTarget.request;
    });
    return () => cancelAnimationFrame(frame);
  }, [searchTarget, key, loading, historyError, messages]);

  useEffect(() => {
    if (!highlightedKey) return;
    const timer = window.setTimeout(() => setHighlightedKey(null), 2800);
    return () => clearTimeout(timer);
  }, [highlightedKey]);

  // initialTopMostItemIndex is an initial condition, not a reactive scroll target.
  // Later ledger updates or row measurements must not change it mid-mount.
  const searchRequest =
    searchTarget?.key === key && "reference" in searchTarget
      ? searchTarget.request
      : null;
  if (messages.length === 0) initialList.current = null;
  else if (
    initialList.current?.key !== key ||
    initialList.current?.searchRequest !== searchRequest
  ) {
    const anchor = anchors.current.get(key);
    const searchIndex =
      searchTarget?.key === key && "reference" in searchTarget
        ? messages.findIndex(
            (message) =>
              message.id !== null &&
              message.id === searchTarget.reference.message_id,
          )
        : -1;
    initialList.current = {
      key,
      searchRequest,
      index:
        searchIndex >= 0
          ? searchIndex
          : anchor
            ? Math.max(
                0,
                messages.findIndex((m, i) => messageRowKey(m, i) === anchor.id),
              )
            : exactScrollPositions.current.has(key)
              ? 0
              : Math.min(
                  messages.length - 1,
                  scrollPositions.current.get(key) ?? messages.length - 1,
                ),
    };
  }

  return {
    virtuosoRef,
    highlightedKey,
    searchMiss,
    showJump,
    scrollerRef(element: HTMLElement | Window | null) {
      scrollerRef.current = element instanceof HTMLElement ? element : null;
    },
    stopRestoring() {
      userInteractionUntil.current = performance.now() + 250;
      pendingUserAnchor.current = true;
      pendingScrollIntent.current = true;
      if (restoringScroll.current === key) restoringScroll.current = null;
    },
    initialIndex: initialList.current?.index ?? 0,
    atBottomStateChange(bottom: boolean) {
      // Virtuoso can report a previous measurement after a long row changes height.
      // The DOM scroller is the source of truth for whether the reader scrolled up.
      const observedBottom = scrollerRef.current
        ? isAtBottom(scrollerRef.current)
        : bottom;
      if (restoringScroll.current === key) {
        atBottom.current.set(key, observedBottom);
        setShowJump(!observedBottom);
        return;
      }
      if (
        !observedBottom &&
        atBottom.current.get(key) !== false &&
        !anchors.current.has(key) &&
        performance.now() >= userInteractionUntil.current
      ) {
        // Measurement drift is not a reader scroll, but the actual gap still
        // needs a usable jump control until live-edge correction completes.
        setShowJump(true);
        return;
      }
      atBottom.current.set(key, observedBottom);
      if (observedBottom) {
        anchors.current.delete(key);
        scrollPositions.current.delete(key);
        exactScrollPositions.current.delete(key);
      } else {
        scrollPositions.current.set(key, lastVisibleRows.current.get(key) ?? 0);
      }
      setShowJump(!observedBottom);
    },
    rangeChanged(startIndex: number) {
      lastVisibleRows.current.set(key, startIndex);
      if (atBottom.current.get(key) === false)
        scrollPositions.current.set(key, startIndex);
    },
    onScroll(event: React.UIEvent<HTMLElement>) {
      if (restoringScroll.current === key) {
        const top = exactScrollPositions.current.get(key);
        if (top !== undefined) {
          const scroller = event.currentTarget;
          const maxTop = Math.max(
            0,
            scroller.scrollHeight - scroller.clientHeight,
          );
          const target = Math.min(top, maxTop);
          if (Math.abs(scroller.scrollTop - target) > 1)
            scroller.scrollTop = target;
        }
        return;
      }
      const scroller = event.currentTarget;
      pinLiveEdge();
      const anchor = anchors.current.get(key);
      const bounds = scroller.getBoundingClientRect();
      const geometryChanged =
        !!anchor &&
        (anchor.viewportTop !== bounds.top ||
          anchor.viewportHeight !== bounds.bottom - bounds.top);
      // Retain the first scroll intent until a visible anchor is captured: a
      // busy event loop can deliver the scroll well after the momentum window.
      const userScrolling =
        pendingScrollIntent.current ||
        pendingUserAnchor.current ||
        performance.now() < userInteractionUntil.current;
      // A browser or virtualizer adjustment is not a new reading position.
      // Restore before observing rows when the viewport changed under the reader.
      if (geometryChanged || (!userScrolling && anchor)) restoreVisibleAnchor();
      if (userScrolling && !geometryChanged) {
        anchors.current.delete(key);
        pendingUserAnchor.current = true;
      }
      measureRows(userScrolling && !geometryChanged);
      pendingScrollIntent.current = false;
      if (userScrolling) userInteractionUntil.current = performance.now() + 250;
      const bottom = isAtBottom(scroller);
      atBottom.current.set(key, bottom);
      setShowJump(!bottom);
      if (bottom) {
        pendingUserAnchor.current = false;
        pendingScrollIntent.current = false;
        anchors.current.delete(key);
        scrollPositions.current.delete(key);
        exactScrollPositions.current.delete(key);
        return;
      }
      exactScrollPositions.current.delete(key);
      exactScrollPositions.current.set(key, scroller.scrollTop);
      if (exactScrollPositions.current.size > 24)
        exactScrollPositions.current.delete(
          exactScrollPositions.current.keys().next().value!,
        );
    },
    jumpToMessage(id: string) {
      const index = messages.findIndex((m, i) => messageRowKey(m, i) === id);
      if (index >= 0) {
        restoringScroll.current = null;
        pendingUserAnchor.current = false;
        pendingScrollIntent.current = false;
        atBottom.current.set(key, false);
        anchors.current.delete(key);
        setHighlightedKey(id);
        virtuosoRef.current?.scrollToIndex({
          index,
          align: "center",
          behavior: "auto",
        });
        return true;
      }
      return false;
    },
    jumpToLatest() {
      pendingUserAnchor.current = false;
      pendingScrollIntent.current = false;
      atBottom.current.set(key, true);
      anchors.current.delete(key);
      if (restoringScroll.current === key) restoringScroll.current = null;
      jumpToLatest(() => virtuosoRef.current);
    },
  };
}
