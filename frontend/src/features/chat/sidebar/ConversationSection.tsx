import type { Ref } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { ConversationRow } from "./ConversationRow";
import type { ConversationListRow } from "./conversationList";

const VIRTUALIZE_AT = 80;
const ROW_HEIGHT = 56;

/** One renderer preserves identical row actions and positions at either list size. */
export function ConversationSection({
  rows,
  firstPosition,
  listSize,
  scrollParent,
  listRef,
  onRangeChanged,
  onTogglePin,
  onRename,
  onHide,
}: {
  rows: readonly ConversationListRow[];
  firstPosition: number;
  listSize: number;
  scrollParent: HTMLElement | null;
  listRef: Ref<VirtuosoHandle>;
  onRangeChanged: (
    range: { startIndex: number; endIndex: number },
    firstPosition: number,
  ) => void;
  onTogglePin: (id: string, pinned: boolean) => void;
  onRename: (id: string, name: string) => void;
  onHide: (id: string, hidden: boolean) => void;
}) {
  const virtual = rows.length > VIRTUALIZE_AT;
  const renderRow = (index: number, row: ConversationListRow) => (
    <ConversationRow
      key={row.id}
      conv={row.conv}
      subtitle={row.subtitle}
      prioritizeSubtitle={row.prioritizeSubtitle}
      channel={row.channel}
      personal={row.personal}
      pinned={row.pinned}
      navIndex={virtual ? index : undefined}
      listPosition={firstPosition + index}
      listSize={listSize}
      onTogglePin={() => onTogglePin(row.id, !row.pinned)}
      onRename={() => onRename(row.id, row.renameName)}
      onHide={row.channel ? () => onHide(row.id, true) : undefined}
    />
  );
  return virtual ? (
    <Virtuoso
      ref={listRef}
      data={rows}
      customScrollParent={scrollParent ?? undefined}
      defaultItemHeight={ROW_HEIGHT}
      overscan={250}
      style={{ height: rows.length * ROW_HEIGHT }}
      computeItemKey={(_, row) => row.id}
      rangeChanged={(range) => onRangeChanged(range, firstPosition)}
      itemContent={renderRow}
    />
  ) : (
    <>{rows.map((row, index) => renderRow(index, row))}</>
  );
}
