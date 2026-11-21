import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";

// A Windows UA exercises selection on every CI host. Native Windows visual
// acceptance remains separate; this verifies the actual bundled font shaper.
test.use({
  userAgent:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36",
});

test("country flags load locally and retain Unicode through the composer", async ({
  page,
}) => {
  const fonts: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("TwemojiCountryFlags"))
      fonts.push(request.url());
  });
  await enterChat(page);
  await openBobDm(page);
  const text = "日本 🇯🇵 Україна 🇺🇦 hello 😀";
  const input = page.getByTestId("composer-input");
  await input.fill(text);
  await expect(input).toHaveValue(text);
  expect(
    await input.evaluate((element) => getComputedStyle(element).fontFamily),
  ).toMatch(/^"?Mesh Talk Country Flags/);
  const shaped = await page.evaluate(async () => {
    await document.fonts.load('32px "Mesh Talk Country Flags"', "🇯🇵🇺🇦");
    const canvas = document.createElement("canvas");
    const ctx = canvas.getContext("2d")!;
    ctx.font = '32px "Mesh Talk Country Flags"';
    return ["🇯🇵", "🇺🇦"].map((flag) => {
      const [first, second] = [...flag];
      return (
        ctx.measureText(flag).width <
        ctx.measureText(first).width + ctx.measureText(second).width
      );
    });
  });
  expect(shaped).toEqual([true, true]);
  expect(fonts.length).toBeGreaterThan(0);
  expect(
    fonts.every((url) => new URL(url).origin === new URL(page.url()).origin),
  ).toBe(true);
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByTestId("message-bubble").filter({ hasText: text }),
  ).toBeVisible();
});

test("font failure preserves readable message text and sending", async ({
  page,
}) => {
  await page.route("**/*TwemojiCountryFlags*.woff2*", (route) => route.abort());
  await enterChat(page);
  await openBobDm(page);
  const text = "Fallback 🇯🇵🇺🇦 中文";
  await page.getByTestId("composer-input").fill(text);
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByTestId("message-bubble").filter({ hasText: text }),
  ).toBeVisible();
});

for (const [platform, userAgent] of [
  ["macOS", "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36"],
  ["Linux", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"],
]) {
  test.describe(platform, () => {
    test.use({ userAgent });
    test("keeps native fonts without requesting the Windows font", async ({
      page,
    }) => {
      const fonts: string[] = [];
      page.on("request", (request) => {
        if (request.url().includes("TwemojiCountryFlags"))
          fonts.push(request.url());
      });
      await enterChat(page);
      await openBobDm(page);
      const input = page.getByTestId("composer-input");
      await input.fill("🇯🇵🇺🇦");
      await input.evaluate(() => document.fonts.ready);
      expect(
        await input.evaluate((element) => getComputedStyle(element).fontFamily),
      ).not.toContain("Mesh Talk Country Flags");
      expect(fonts).toEqual([]);
    });
  });
}
