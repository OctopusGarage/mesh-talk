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
  await page.getByRole("button", { name: "NBA", exact: true }).click();
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

test("slow avatar selection has readable progress with reduced motion", async ({
  page,
}) => {
  await seedMarketPacks(page, ["players"]);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await enterChat(page);
  await page.getByTestId("open-profile").click();
  await page.getByRole("button", { name: "Change your photo" }).click();
  await page.getByText(/Choose from gallery/).click();
  await expect(page.getByTestId("avatar-gallery")).toBeVisible();

  // Hold decoding until progress is inspected; a timed delay can finish between
  // assertions when the browser is instrumented or the machine is busy.
  await page.evaluate(() => {
    const src = Object.getOwnPropertyDescriptor(
      HTMLImageElement.prototype,
      "src",
    );
    if (!src?.set) throw new Error("Image src setter unavailable");
    const pending: (() => void)[] = [];
    const win = window as typeof window & {
      __releaseAvatarDecode?: () => void;
    };
    win.__releaseAvatarDecode = () => {
      Object.defineProperty(HTMLImageElement.prototype, "src", src);
      for (const decode of pending) decode();
      delete win.__releaseAvatarDecode;
    };
    Object.defineProperty(HTMLImageElement.prototype, "src", {
      configurable: true,
      get: src.get,
      set(this: HTMLImageElement, value: string) {
        if (this.isConnected) src.set!.call(this, value);
        else pending.push(() => src.set!.call(this, value));
      },
    });
  });
  const choice = page.getByTestId("avatar-gallery").getByRole("button").first();
  try {
    await choice.click();
    await expect(choice).toHaveAttribute("aria-busy", "true");
    await expect(choice.getByText("Loading…")).toBeVisible();
    await expect(choice.locator("svg")).toBeHidden();
  } finally {
    await page.evaluate(() => {
      const win = window as typeof window & {
        __releaseAvatarDecode?: () => void;
      };
      win.__releaseAvatarDecode?.();
    });
  }
  await expect(page.getByTestId("avatar-gallery")).toHaveCount(0);
});
