import { afterEach, expect, it, vi } from "vitest";
import { scrollToSettingsSection, settingsCategory } from "./settingsScroll";

it("keeps related settings under a single navigation category", () => {
  expect(settingsCategory("contacts")).toBe("privacy");
  expect(settingsCategory("files")).toBe("files");
  expect(settingsCategory("background")).toBe("background");
});

afterEach(() => vi.unstubAllGlobals());

function section() {
  const scrollIntoView = vi.fn();
  const querySelector = vi.fn().mockReturnValue({ scrollIntoView });
  return {
    container: { querySelector } as unknown as Pick<
      HTMLElement,
      "querySelector"
    >,
    querySelector,
    scrollIntoView,
  };
}

it("scrolls to the requested settings section with normal motion", () => {
  vi.stubGlobal("window", {
    matchMedia: () => ({ matches: false }),
  });
  const { container, querySelector, scrollIntoView } = section();

  scrollToSettingsSection(container, "privacy");

  expect(querySelector).toHaveBeenCalledWith("#settings-privacy");
  expect(scrollIntoView).toHaveBeenCalledWith({
    block: "start",
    behavior: "smooth",
  });
});

it("respects reduced motion when navigating settings", () => {
  vi.stubGlobal("window", {
    matchMedia: () => ({ matches: true }),
  });
  const { container, scrollIntoView } = section();

  scrollToSettingsSection(container, "files");

  expect(scrollIntoView).toHaveBeenCalledWith({
    block: "start",
    behavior: "auto",
  });
});

it("does nothing when the settings section is unavailable", () => {
  const querySelector = vi.fn().mockReturnValue(null);
  const container = { querySelector } as unknown as Pick<
    HTMLElement,
    "querySelector"
  >;

  expect(() => scrollToSettingsSection(container, "missing")).not.toThrow();
  expect(() => scrollToSettingsSection(null, "missing")).not.toThrow();
});
