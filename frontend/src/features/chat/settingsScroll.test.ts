import { afterEach, expect, it, vi } from "vitest";
import { scrollToSettingsSection, settingsCategory } from "./settingsScroll";

it("keeps related settings under a single navigation category", () => {
  expect(settingsCategory("contacts")).toBe("privacy");
  expect(settingsCategory("files")).toBe("files");
  expect(settingsCategory("background")).toBe("background");
});

afterEach(() => vi.unstubAllGlobals());

function section() {
  const scrollTo = vi.fn();
  const querySelector = vi.fn().mockReturnValue({
    getBoundingClientRect: () => ({ top: 250 }),
  });
  return {
    container: {
      querySelector,
      getBoundingClientRect: () => ({ top: 100 }),
      scrollTop: 120,
      scrollTo,
    } as unknown as Pick<
      HTMLElement,
      "querySelector" | "getBoundingClientRect" | "scrollTop" | "scrollTo"
    >,
    querySelector,
    scrollTo,
  };
}

it("scrolls to the requested settings section with normal motion", () => {
  vi.stubGlobal("window", {
    matchMedia: () => ({ matches: false }),
  });
  const { container, querySelector, scrollTo } = section();

  scrollToSettingsSection(container, "privacy");

  expect(querySelector).toHaveBeenCalledWith("#settings-privacy");
  expect(scrollTo).toHaveBeenCalledWith({
    top: 270,
    behavior: "smooth",
  });
});

it("respects reduced motion when navigating settings", () => {
  vi.stubGlobal("window", {
    matchMedia: () => ({ matches: true }),
  });
  const { container, scrollTo } = section();

  scrollToSettingsSection(container, "files");

  expect(scrollTo).toHaveBeenCalledWith({
    top: 270,
    behavior: "auto",
  });
});

it("does nothing when the settings section is unavailable", () => {
  const querySelector = vi.fn().mockReturnValue(null);
  const scrollTo = vi.fn();
  const container = { querySelector, scrollTo } as unknown as HTMLElement;

  expect(() => scrollToSettingsSection(container, "missing")).not.toThrow();
  expect(() => scrollToSettingsSection(null, "missing")).not.toThrow();
  expect(scrollTo).not.toHaveBeenCalled();
});
