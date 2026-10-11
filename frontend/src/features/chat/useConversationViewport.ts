import { useEffect, useRef, useState } from "react";
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
) {
  const [showJump, setShowJump] = useState(false);
  const [highlightedKey, setHighlightedKey] = useState<string | null>(null);
  const [searchMiss, setSearchMiss] = useState(false);
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const scrollerRef = useRef<HTMLElement | null>(null);
  const scrollPositions = useRef(new Map<string, number>());
  const exactScrollPositions = useRef(new Map<string, number>());
  const restoringScroll = useRef<string | null>(null);
  const lastVisibleRows = useRef(new Map<string, number>());
  const atBottom = useRef(new Map<string, boolean>());
  const handledSearch = useRef<number | null>(null);

  useEffect(() => {
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
    const top = exactScrollPositions.current.get(key);
    if (top === undefined) return;
    let frame = 0;
    let attempts = 0;
    const restore = () => {
      const scroller = scrollerRef.current;
      const maxTop = scroller
        ? scroller.scrollHeight - scroller.clientHeight
        : 0;
      if ((!scroller || maxTop < top) && attempts++ < 30) {
        frame = requestAnimationFrame(restore);
        return;
      }
      if (scroller) {
        scroller.scrollTop = Math.min(top, Math.max(0, maxTop));
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
    if (handledSearch.current === searchTarget.request) return;
    const index = messages.findIndex(
      (message) =>
        message.wallClock === searchTarget.wallClock &&
        message.text === searchTarget.text &&
        message.fromMe === searchTarget.fromMe,
    );
    if (index < 0) {
      handledSearch.current = searchTarget.request;
      setSearchMiss(true);
      return;
    }
    setSearchMiss(false);
    setHighlightedKey(messageRowKey(messages[index], index));
    const frame = requestAnimationFrame(() => {
      if (restoringScroll.current === key) restoringScroll.current = null;
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

  return {
    virtuosoRef,
    highlightedKey,
    searchMiss,
    showJump,
    scrollerRef(element: HTMLElement | Window | null) {
      scrollerRef.current = element instanceof HTMLElement ? element : null;
    },
    stopRestoring() {
      if (restoringScroll.current === key) restoringScroll.current = null;
    },
    // Exact pixel restoration runs after the list has measured its rows. Starting
    // Virtuoso at the saved row as well can issue a later scroll that overwrites it.
    initialIndex: exactScrollPositions.current.has(key)
      ? 0
      : Math.min(
          messages.length - 1,
          scrollPositions.current.get(key) ?? messages.length - 1,
        ),
    atBottomStateChange(bottom: boolean) {
      // Virtuoso can report a previous measurement after a long row changes height.
      // The DOM scroller is the source of truth for whether the reader scrolled up.
      const observedBottom = scrollerRef.current
        ? isAtBottom(scrollerRef.current)
        : bottom;
      atBottom.current.set(key, observedBottom);
      if (restoringScroll.current === key) return;
      if (observedBottom) {
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
      const bottom = isAtBottom(scroller);
      atBottom.current.set(key, bottom);
      setShowJump(!bottom);
      if (bottom) {
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
    jumpToLatest() {
      if (restoringScroll.current === key) restoringScroll.current = null;
      jumpToLatest(() => virtuosoRef.current);
    },
  };
}
