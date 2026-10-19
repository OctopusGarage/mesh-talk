import { useCallback, useRef, useState } from "react";
import type { VirtuosoHandle } from "react-virtuoso";

/** Focus/scroll ownership is independent of row presentation and list projection. */
export function useConversationNavigation(
  pinnedCount: number,
  accountCount: number,
  listSize: number,
) {
  // Roving keyboard navigation across the conversation rows. Arrow Up/Down moves focus
  // between the option buttons within the list (Enter/Space already open via the native
  // button). Scoped to the nav so it never traps the composer or other controls.
  const navRef = useRef<HTMLElement>(null);
  const [navScrollParent, setNavScrollParent] = useState<HTMLElement | null>(
    null,
  );
  const virtualPinnedRef = useRef<VirtuosoHandle>(null);
  const virtualAccountsRef = useRef<VirtuosoHandle>(null);
  const virtualChannelsRef = useRef<VirtuosoHandle>(null);
  const pendingVirtualFocus = useRef<number | null>(null);
  const attachNav = useCallback((node: HTMLElement | null) => {
    navRef.current = node;
    setNavScrollParent(node);
  }, []);
  const onNavKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    if (
      !(e.target instanceof HTMLButtonElement) ||
      !e.target.matches("[data-conv-option]")
    )
      return;
    const position = Number(e.target.dataset.navPosition);
    const next = Math.max(
      1,
      Math.min(listSize, position + (e.key === "ArrowDown" ? 1 : -1)),
    );
    e.preventDefault();
    const visible = navRef.current?.querySelector<HTMLButtonElement>(
      `[data-nav-position="${next}"]`,
    );
    if (visible) {
      pendingVirtualFocus.current = null;
      visible.focus();
      return;
    }
    pendingVirtualFocus.current = next;
    if (next <= pinnedCount) {
      virtualPinnedRef.current?.scrollToIndex({
        index: next - 1,
        align: "center",
      });
    } else if (next <= pinnedCount + accountCount) {
      virtualAccountsRef.current?.scrollToIndex({
        index: next - pinnedCount - 1,
        align: "center",
      });
    } else {
      virtualChannelsRef.current?.scrollToIndex({
        index: next - pinnedCount - accountCount - 1,
        align: "center",
      });
    }
  };

  const focusVirtualRow = (
    {
      startIndex,
      endIndex,
    }: {
      startIndex: number;
      endIndex: number;
    },
    firstPosition: number,
  ) => {
    const pending = pendingVirtualFocus.current;
    if (
      pending === null ||
      pending < firstPosition + startIndex ||
      pending > firstPosition + endIndex
    )
      return;
    requestAnimationFrame(() => {
      if (pendingVirtualFocus.current !== pending) return;
      const target = navRef.current?.querySelector<HTMLButtonElement>(
        `[data-nav-position="${pending}"]`,
      );
      if (
        target &&
        (document.activeElement === document.body ||
          navRef.current?.contains(document.activeElement))
      ) {
        target.focus();
        pendingVirtualFocus.current = null;
      }
    });
  };

  return {
    attachNav,
    navScrollParent,
    onNavKeyDown,
    virtualPinnedRef,
    virtualAccountsRef,
    virtualChannelsRef,
    focusVirtualRow,
    cancelPendingFocus: () => {
      pendingVirtualFocus.current = null;
    },
  };
}
