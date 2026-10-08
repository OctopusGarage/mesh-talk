import { useCallback, useRef, type ReactNode } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";

export const VIRTUALIZE_AT = 80;
const ROW_HEIGHT = 48;

interface VirtualRosterListProps<T> {
  items: T[];
  maxHeight: number;
  className: string;
  itemKey: (item: T) => string;
  renderItem: (item: T, index: number) => ReactNode;
  rowHeight?: number;
  focusSelector?: string;
}

/** Keeps long rosters bounded while preserving keyboard access to distant rows. */
export function VirtualRosterList<T>({
  items,
  maxHeight,
  className,
  itemKey,
  renderItem,
  rowHeight = ROW_HEIGHT,
  focusSelector = "button",
}: VirtualRosterListProps<T>) {
  const listRef = useRef<VirtuosoHandle>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const pendingFocus = useRef<number | null>(null);

  const focusPending = useCallback(() => {
    const index = pendingFocus.current;
    if (index === null) return;
    const target = containerRef.current?.querySelector<HTMLElement>(
      `[data-virtual-index="${index}"] ${focusSelector}`,
    );
    if (target) {
      target.focus();
      pendingFocus.current = null;
    }
  }, [focusSelector]);

  if (items.length <= VIRTUALIZE_AT) {
    return (
      <div className={className} style={{ maxHeight }}>
        {items.map((item, index) => (
          <div key={itemKey(item)}>{renderItem(item, index)}</div>
        ))}
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      className={className}
      style={{ height: maxHeight }}
      onPointerDownCapture={() => {
        pendingFocus.current = null;
      }}
      onKeyDown={(event) => {
        if (event.key === "Tab") {
          pendingFocus.current = null;
          return;
        }
        if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key))
          return;
        if (!(event.target as HTMLElement).matches(focusSelector)) return;
        const row = (event.target as HTMLElement).closest<HTMLElement>(
          "[data-virtual-index]",
        );
        if (!row) return;
        const current = Number(row.dataset.virtualIndex);
        const next =
          event.key === "Home"
            ? 0
            : event.key === "End"
              ? items.length - 1
              : Math.max(
                  0,
                  Math.min(
                    items.length - 1,
                    current + (event.key === "ArrowDown" ? 1 : -1),
                  ),
                );
        if (next === current) return;
        event.preventDefault();
        pendingFocus.current = next;
        listRef.current?.scrollToIndex({ index: next, align: "center" });
        requestAnimationFrame(focusPending);
      }}
    >
      <Virtuoso
        ref={listRef}
        data={items}
        style={{ height: "100%" }}
        defaultItemHeight={rowHeight}
        overscan={rowHeight * 3}
        computeItemKey={(_, item) => itemKey(item)}
        rangeChanged={focusPending}
        itemContent={(index, item) => (
          <div data-virtual-index={index}>{renderItem(item, index)}</div>
        )}
      />
    </div>
  );
}
