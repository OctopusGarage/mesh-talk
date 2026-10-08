export function settingsScrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "auto"
    : "smooth";
}

/** Related setting sections share one navigation destination. */
export function settingsCategory(section: string): string {
  if (section === "contacts") return "privacy";
  return section;
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
