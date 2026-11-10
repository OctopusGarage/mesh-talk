/** Preserve a newer connected focus owner when Radix releases an older scope. */
export function preserveNewerFocusOnClose(event: Event) {
  if (event.defaultPrevented) return;
  // FocusScope dispatches this event on the closing DOM element, after unmount.
  const closing = event.target as Element;
  const ownerDocument = closing.ownerDocument;
  const active = ownerDocument.activeElement;
  if (
    active?.isConnected &&
    active !== ownerDocument.body &&
    active !== ownerDocument.documentElement &&
    !active.hasAttribute("data-radix-focus-guard") &&
    !closing.contains(active)
  )
    event.preventDefault();
}
