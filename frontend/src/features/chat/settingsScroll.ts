export function settingsScrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "auto"
    : "smooth";
}

export function scrollToSettingsSection(
  container: Pick<HTMLElement, "querySelector"> | null,
  id: string,
): void {
  container?.querySelector(`#settings-${id}`)?.scrollIntoView({
    block: "start",
    behavior: settingsScrollBehavior(),
  });
}
