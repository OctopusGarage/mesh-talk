import type { Locator, Page } from "@playwright/test";
import { expect } from "../tauri-mock";

export async function expectNoHorizontalOverflow(page: Page, label: string) {
  const metrics = await page.evaluate(() => ({
    documentScrollWidth: document.documentElement.scrollWidth,
    documentClientWidth: document.documentElement.clientWidth,
    bodyScrollWidth: document.body.scrollWidth,
    bodyClientWidth: document.body.clientWidth,
  }));
  expect(
    metrics.documentScrollWidth,
    `${label}: document overflows horizontally`,
  ).toBeLessThanOrEqual(metrics.documentClientWidth + 1);
  expect(
    metrics.bodyScrollWidth,
    `${label}: body overflows horizontally`,
  ).toBeLessThanOrEqual(metrics.bodyClientWidth + 1);
}

export async function expectElementsWithin(
  page: Page,
  childSelector: string,
  parentSelector: string,
) {
  const failures = await page.evaluate(
    ({ childSelector, parentSelector }) => {
      const parent = document.querySelector(parentSelector) as HTMLElement | null;
      if (!parent) return [`missing parent ${parentSelector}`];
      const pr = parent.getBoundingClientRect();
      return Array.from(document.querySelectorAll<HTMLElement>(childSelector))
        .map((child) => {
          const cr = child.getBoundingClientRect();
          const id =
            child.getAttribute("data-testid") ||
            child.getAttribute("aria-label") ||
            child.tagName;
          const outside =
            cr.left < pr.left - 1 ||
            cr.right > pr.right + 1 ||
            cr.top < pr.top - 1 ||
            cr.bottom > pr.bottom + 1;
          return outside
            ? `${id} outside ${parentSelector}: child=${JSON.stringify({
                left: cr.left,
                right: cr.right,
                top: cr.top,
                bottom: cr.bottom,
              })} parent=${JSON.stringify({
                left: pr.left,
                right: pr.right,
                top: pr.top,
                bottom: pr.bottom,
              })}`
            : null;
        })
        .filter(Boolean);
    },
    { childSelector, parentSelector },
  );
  expect(failures).toEqual([]);
}

export async function expectMinTargetSize(locator: Locator, minPx = 32) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, "target has no bounding box").not.toBeNull();
  expect(box!.width, "target width too small").toBeGreaterThanOrEqual(minPx);
  expect(box!.height, "target height too small").toBeGreaterThanOrEqual(minPx);
}

export async function expectVisibleFocus(page: Page, locator: Locator) {
  await locator.focus();
  const focus = await locator.evaluate((el) => {
    const style = getComputedStyle(el);
    return {
      outlineStyle: style.outlineStyle,
      outlineWidth: style.outlineWidth,
      boxShadow: style.boxShadow,
      active: el.matches(":focus-visible") || el === document.activeElement,
    };
  });
  expect(focus.active).toBe(true);
  expect(
    focus.boxShadow !== "none" ||
      (focus.outlineStyle !== "none" && focus.outlineWidth !== "0px"),
    "focused element has no visible focus treatment",
  ).toBe(true);
  await page.keyboard.press("Escape");
}

export function contrastRatio(foreground: string, background: string) {
  const parseRgb = (value: string) => {
    const channels = value.match(/[\d.]+/g)?.map(Number);
    if (!channels || channels.length < 3) {
      throw new Error(`Cannot parse color: ${value}`);
    }
    return channels.slice(0, 3).map((channel) => channel / 255);
  };
  const luminance = (value: string) => {
    const [red, green, blue] = parseRgb(value).map((channel) =>
      channel <= 0.03928
        ? channel / 12.92
        : ((channel + 0.055) / 1.055) ** 2.4,
    );
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
  };
  const fg = luminance(foreground);
  const bg = luminance(background);
  return (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
}

export async function expectDialogFitsViewport(page: Page, dialogTestId: string) {
  const result = await page.getByTestId(dialogTestId).evaluate((dialog) => {
    const rect = dialog.getBoundingClientRect();
    return {
      left: rect.left,
      right: rect.right,
      top: rect.top,
      bottom: rect.bottom,
      width: window.innerWidth,
      height: window.innerHeight,
    };
  });
  expect(result.left, `${dialogTestId} clips left`).toBeGreaterThanOrEqual(8);
  expect(result.top, `${dialogTestId} clips top`).toBeGreaterThanOrEqual(8);
  expect(result.right, `${dialogTestId} clips right`).toBeLessThanOrEqual(
    result.width - 8,
  );
  expect(result.bottom, `${dialogTestId} clips bottom`).toBeLessThanOrEqual(
    result.height - 8,
  );
}
