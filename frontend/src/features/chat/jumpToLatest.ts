import type { VirtuosoHandle } from "react-virtuoso";

export function jumpToLatest(
  getList: () => Pick<VirtuosoHandle, "scrollTo"> | null,
  schedule: (callback: () => void) => void = (callback) => {
    requestAnimationFrame(callback);
  },
): void {
  const list = getList();
  if (!list) return;
  const scrollToBottom = () => {
    if (getList() !== list) return;
    list.scrollTo({ top: Number.MAX_SAFE_INTEGER, behavior: "auto" });
  };
  scrollToBottom();
  schedule(scrollToBottom);
}
