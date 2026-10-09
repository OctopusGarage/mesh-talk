import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { seedMarketPacks } from "./helpers/packs";

test.use({ viewport: { width: 760, height: 520 } });

test("gallery loads thumbnails after scrolling to them", async ({ page }) => {
  await seedMarketPacks(page, ["nba-players"]);
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await page.getByRole("button", { name: "NBA stars", exact: true }).click();
  const gallery = page.getByTestId("avatar-gallery");
  await expect(gallery.locator("img")).toHaveCount(60);

  await gallery.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  const last = gallery.locator("img").last();
  await expect(last).toHaveAttribute("src", /^data:image\/webp;base64,/);
  await expect
    .poll(() =>
      last.evaluate((image) => (image as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(1);
});
