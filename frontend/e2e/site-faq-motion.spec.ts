import { expect, test } from "@playwright/test";
import { servePublicSite } from "./helpers/public-site";

let hosted: Awaited<ReturnType<typeof servePublicSite>>;
test.beforeAll(async () => {
  hosted = await servePublicSite();
});
test.afterAll(async () => {
  await hosted?.close();
});
test.beforeEach(async ({ page }) => {
  await page.route("https://api.github.com/repos/**", (route) =>
    route.fulfill({ status: 503, json: { message: "Offline fixture" } }),
  );
});

for (const reducedMotion of ["no-preference", "reduce"] as const) {
  test(`native keyboard disclosures remain usable with ${reducedMotion}`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion });
    await page.goto(`${hosted.url}?lang=en`);
    const answer = page.locator("#faq-offline");
    const summary = answer.locator("summary");
    await summary.focus();
    await page.keyboard.press("Enter");
    await expect(answer.locator("p")).toBeVisible();
    await expect
      .poll(() => answer.evaluate((element) => element.getAnimations().length))
      .toBe(0);
    expect(
      await answer.evaluate(
        (element) => element.scrollHeight <= element.clientHeight + 1,
      ),
    ).toBe(true);
    await expect(summary).toBeFocused();
    await page.keyboard.press("Space");
    await expect(answer).not.toHaveAttribute("open");
    await expect(answer.locator("p")).toBeHidden();
    await expect(summary).toBeFocused();
    if (reducedMotion === "reduce")
      expect(
        await answer.evaluate((element) => element.getAnimations().length),
      ).toBe(0);
  });
}

test("rapid reversals continue from the currently displayed height", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(`${hosted.url}?lang=en`);
  const answer = page.locator("#faq-offline");
  const reversal = await answer.evaluate(async (element) => {
    const summary = element.querySelector("summary")!;
    const closed = element.getBoundingClientRect().height;
    summary.click();
    // Sample partway through the authored 270 ms expansion.
    await new Promise((resolve) => setTimeout(resolve, 70));
    const beforeClose = element.getBoundingClientRect().height;
    summary.click();
    const afterClose = element.getBoundingClientRect().height;
    await new Promise((resolve) => setTimeout(resolve, 40));
    const beforeReopen = element.getBoundingClientRect().height;
    summary.click();
    const afterReopen = element.getBoundingClientRect().height;
    return { closed, beforeClose, afterClose, beforeReopen, afterReopen };
  });
  expect(reversal.beforeClose).toBeGreaterThan(reversal.closed + 1);
  expect(Math.abs(reversal.beforeClose - reversal.afterClose)).toBeLessThan(2);
  expect(Math.abs(reversal.beforeReopen - reversal.afterReopen)).toBeLessThan(
    2,
  );
  await expect
    .poll(() => answer.evaluate((element) => element.getAnimations().length))
    .toBe(0);
  await expect(answer).toHaveAttribute("open", "");
  expect(
    await answer.evaluate(
      (element) => element.scrollHeight <= element.clientHeight + 1,
    ),
  ).toBe(true);
});

test("closing answers immediately remove their links from keyboard focus", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(`${hosted.url}?lang=en#faq-offline`);
  const answer = page.locator("#faq-offline");
  const result = await answer.evaluate((element) => {
    element.querySelector("summary")!.click();
    const link = element.querySelector("a")!;
    link.focus();
    return {
      focusable: document.activeElement === link,
      moving: element.getAnimations().length > 0,
    };
  });
  expect(result.moving).toBe(true);
  expect(result.focusable).toBe(false);
  await expect(answer).not.toHaveAttribute("open");
});

test("direct fragments and answer links preserve native disclosure navigation", async ({
  page,
}) => {
  await page.goto(`${hosted.url}?lang=en#faq-networks`);
  const answer = page.locator("#faq-networks");
  await expect(answer).toHaveAttribute("open", "");
  await expect(answer.locator("p")).toBeVisible();
  await answer.locator('a[href="#networks"]').click();
  expect(new URL(page.url()).hash).toBe("#networks");
  await expect(answer).toHaveAttribute("open", "");

  await page.evaluate(() => {
    const summary = document
      .querySelector<HTMLDetailsElement>("#faq-networks")!
      .querySelector("summary")!;
    summary.click();
    location.hash = "faq-networks";
  });
  await expect(answer).toHaveAttribute("open", "");
  await expect
    .poll(() => answer.evaluate((element) => element.getAnimations().length))
    .toBe(0);
  await expect(answer.locator("p")).toBeVisible();
});

test("a runtime reduced-motion change settles to the intended disclosure state", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(`${hosted.url}?lang=en#faq-offline`);
  const answer = page.locator("#faq-offline");
  await answer
    .locator("summary")
    .evaluate((element) => (element as HTMLElement).click());
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect(answer).not.toHaveAttribute("open");
  expect(
    await answer.evaluate((element) => element.getAnimations().length),
  ).toBe(0);
  await answer.locator("summary").click();
  await expect(answer).toHaveAttribute("open", "");
  expect(
    await answer.evaluate((element) => element.getAnimations().length),
  ).toBe(0);
});

test("language replacement during expansion cannot clip the translated answer", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(`${hosted.url}?lang=en`);
  // Warm the real dictionary so replacement occurs during the next expansion.
  await page.locator("#site-language").selectOption("ja");
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  await page.locator("#site-language").selectOption("en");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await page.evaluate(() => {
    document.querySelector<HTMLElement>("#faq-offline summary")!.click();
    const chooser =
      document.querySelector<HTMLSelectElement>("#site-language")!;
    chooser.value = "ja";
    chooser.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  const answer = page.locator("#faq-offline");
  await expect(answer).toHaveAttribute("open", "");
  await expect
    .poll(() => answer.evaluate((element) => element.getAnimations().length))
    .toBe(0);
  expect(
    await answer.evaluate(
      (element) => element.scrollHeight <= element.clientHeight + 1,
    ),
  ).toBe(true);
  await expect(answer.locator("p")).toBeVisible();
  await expect(answer.locator("a").first()).toBeVisible();
});

test("native FAQ interaction remains available without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(hosted.url);
    const answer = page.locator("#faq-offline");
    await answer.locator("summary").click();
    await expect(answer).toHaveAttribute("open", "");
    await expect(answer.locator("p")).toBeVisible();
    await answer.locator("summary").click();
    await expect(answer.locator("p")).toBeHidden();
  } finally {
    await context.close();
  }
});
