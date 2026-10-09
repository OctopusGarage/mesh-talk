import { useEffect, useRef, useState } from "react";
import type { VirtuosoHandle } from "react-virtuoso";
import type { ChatMessage, SearchTarget } from "@/store/chat";
import { jumpToLatest } from "./jumpToLatest";

export const messageRowKey = (message: ChatMessage, index: number) =>
  message.clientId ?? message.id ?? `pending-${index}`;

type Target = (SearchTarget & { key: string; request: number }) | null;

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
        setShowJump(maxTop - scroller.scrollTop > 48);
      }
      restoringScroll.current = null;
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
    // Exact pixel restoration runs after the list has measured its rows. Starting
    // Virtuoso at the saved row as well can issue a later scroll that overwrites it.
    initialIndex: exactScrollPositions.current.has(key)
      ? 0
      : Math.min(
          messages.length - 1,
          scrollPositions.current.get(key) ?? messages.length - 1,
        ),
    atBottomStateChange(bottom: boolean) {
      atBottom.current.set(key, bottom);
      if (restoringScroll.current === key) return;
      if (bottom) {
        scrollPositions.current.delete(key);
        exactScrollPositions.current.delete(key);
      } else {
        scrollPositions.current.set(key, lastVisibleRows.current.get(key) ?? 0);
      }
      setShowJump(!bottom);
    },
    rangeChanged(startIndex: number) {
      lastVisibleRows.current.set(key, startIndex);
      if (atBottom.current.get(key) === false)
        scrollPositions.current.set(key, startIndex);
    },
    onScroll(event: React.UIEvent<HTMLElement>) {
      if (restoringScroll.current === key) return;
      const scroller = event.currentTarget;
      if (
        scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop <=
        48
      ) {
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
      jumpToLatest(() => virtuosoRef.current);
    },
  };
}
