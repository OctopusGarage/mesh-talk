export function settingsScrollBehavior(): ScrollBehavior {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches
    ? "auto"
    : "smooth";
}

/** Related setting sections share one navigation destination. */
export function settingsCategory(section: string): string {
  if (section === "contacts" || section === "security") return "privacy";
  return section;
}

export function scrollToSettingsSection(
  container: Pick<
    HTMLElement,
    "querySelector" | "getBoundingClientRect" | "scrollTop" | "scrollTo"
  > | null,
  id: string,
): void {
  const section = container?.querySelector(`#settings-${id}`);
  if (!container || !section) return;
  const top =
    container.scrollTop +
    section.getBoundingClientRect().top -
    container.getBoundingClientRect().top;
  container.scrollTo({
    top,
    behavior: settingsScrollBehavior(),
  });
}
