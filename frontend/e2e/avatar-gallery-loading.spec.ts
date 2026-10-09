import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test.use({ viewport: { width: 760, height: 520 } });

test("gallery defers offscreen thumbnails until they are scrolled into view", async ({
  page,
}) => {
  const requested = new Set<string>();
  page.on("request", (request) => {
    if (
      request.resourceType() === "image" &&
      request.url().includes("/nba-players/")
    ) {
      requested.add(request.url());
    }
  });

  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await page.getByRole("button", { name: "NBA", exact: true }).click();
  const gallery = page.getByTestId("avatar-gallery");
  await expect(gallery.locator("img")).toHaveCount(60);
  await page.waitForLoadState("networkidle");
  expect(requested.size).toBeLessThan(40);
  const initiallyRequested = requested.size;

  await gallery.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  const last = gallery.locator("img").last();
  await expect(last).toHaveAttribute("src", /nba-players/);
  await expect
    .poll(() =>
      last.evaluate((image) => (image as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(1);
  expect(requested.size).toBeGreaterThan(initiallyRequested);
});
