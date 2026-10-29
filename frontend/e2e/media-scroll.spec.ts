import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB } from "./helpers/session";

// Successful portable scenarios retain evidence in the full Chromium run.
test.use({ trace: "on" });

type MediaScrollProbe = {
  reads: Record<string, number>;
  pending: Array<() => void>;
  released: boolean;
};
type ProbeWindow = Window & {
  __mediaScroll: MediaScrollProbe;
  __TAURI_INTERNALS__: {
    invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  };
};

async function seedImages(
  page: import("@playwright/test").Page,
  count = 60,
  media = { width: 800, height: 600, video: false, invalid: false },
) {
  await enterChat(page);
  await page.evaluate(
    ({ count, media }) => {
      const win = window as unknown as ProbeWindow;
      win.__mediaScroll = { reads: {}, pending: [], released: false };
      const canvas = document.createElement("canvas");
      canvas.width = media.width;
      canvas.height = media.height;
      canvas.getContext("2d")!.fillRect(0, 0, media.width, media.height);
      const png = Uint8Array.from(
        atob(canvas.toDataURL().split(",")[1]),
        (char) => char.charCodeAt(0),
      ).buffer;
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        const probe = win.__mediaScroll;
        if (
          cmd === "read_media" &&
          String(args.fileConv).startsWith("scroll-image-")
        ) {
          const id = String(args.fileConv);
          probe.reads[id] = (probe.reads[id] ?? 0) + 1;
          if (!probe.released)
            await new Promise<void>((resolve) => probe.pending.push(resolve));
          return media.invalid ? new ArrayBuffer(1) : png;
        }
        if (
          cmd === "owner_account_history" &&
          args.account === "acc_bob_bbbb2222"
        ) {
          return Array.from({ length: count }, (_, i) => ({
            id: `scroll-message-${i}`,
            from_me: false,
            who: "device_bob_2222",
            text: "",
            wall_clock: 1700000000000 + i * 1000,
            reply_to: null,
            file: {
              name: `image-${i}.${media.video ? "webm" : "png"}`,
              size: png.byteLength,
              mime: media.video ? "video/webm" : "image/png",
              file_conv: `scroll-image-${i}`,
              media: true,
            },
          }));
        }
        return original(cmd, args);
      };
    },
    { count, media },
  );
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
}
async function releaseImages(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    const probe = (window as unknown as ProbeWindow).__mediaScroll;
    probe.released = true;
    for (const resolve of probe.pending.splice(0)) resolve();
  });
}

test("image rows reserve their final geometry before bytes load", async ({
  page,
}) => {
  await seedImages(page, 1);
  const row = page.locator('[data-message-id="scroll-message-0"]');
  await expect(row.getByRole("status")).toBeVisible();
  const before = await row.boundingBox();
  await releaseImages(page);
  const image = row.getByTestId("file-image");
  await expect
    .poll(() => image.evaluate((el) => (el as HTMLImageElement).naturalWidth))
    .toBe(800);
  await expect
    .poll(
      async () =>
        Math.abs(((await row.boundingBox())?.height ?? 0) - before!.height),
      { message: "Loading must not resize a media row" },
    )
    .toBeLessThanOrEqual(1);
  await image.click();
  await expect(page.getByTestId("file-image-lightbox")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("file-image-lightbox")).toHaveCount(0);
});

test("scrolling away and returning to latest reuses media without changing row height", async ({
  page,
}) => {
  await seedImages(page);
  await releaseImages(page);
  const row = page.locator('[data-message-id="scroll-message-59"]');
  await expect
    .poll(() =>
      row
        .getByTestId("file-image")
        .evaluate((el) => (el as HTMLImageElement).naturalWidth),
    )
    .toBe(800);
  await expect(row).toBeVisible();
  const initialHeight = await row.evaluate(
    (element) => element.getBoundingClientRect().height,
  );
  const readsBefore = await page.evaluate(
    () =>
      (window as unknown as ProbeWindow).__mediaScroll.reads["scroll-image-59"],
  );
  const log = page.getByRole("log");
  const bounds = (await log.boundingBox())!;
  await page.mouse.move(
    bounds.x + bounds.width / 2,
    bounds.y + bounds.height / 2,
  );
  await page.mouse.wheel(0, -3000);
  await expect(row).toHaveCount(0);
  await page.getByTestId("jump-to-latest").click();
  await expect(row.getByTestId("file-image")).toBeVisible();
  await expect
    .poll(async () => (await row.boundingBox())!.height)
    .toBe(initialHeight);
  const readsAfter = await page.evaluate(
    () =>
      (window as unknown as ProbeWindow).__mediaScroll.reads["scroll-image-59"],
  );
  expect(readsAfter, "Virtualized remount must reuse the live media URL").toBe(
    readsBefore,
  );
});

for (const [label, width, height] of [
  ["portrait", 300, 900],
  ["panorama", 1600, 200],
] as const) {
  test(`${label} images retain a stable contained frame in a short window`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 820, height: 620 });
    await seedImages(page, 1, { width, height, video: false, invalid: false });
    const row = page.locator('[data-message-id="scroll-message-0"]');
    await expect(row.getByRole("status")).toBeVisible();
    const before = await row.evaluate(
      (element) => element.getBoundingClientRect().height,
    );
    await releaseImages(page);
    const image = row.getByTestId("file-image");
    await expect
      .poll(() =>
        image.evaluate((element) => (element as HTMLImageElement).naturalWidth),
      )
      .toBe(width);
    expect(
      await row.evaluate((element) => element.getBoundingClientRect().height),
    ).toBe(before);
    await expect(image).toHaveCSS("object-fit", "contain");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
  });
}
for (const video of [false, true]) {
  test(`unsupported ${video ? "video" : "image"} retains its loading geometry`, async ({
    page,
  }) => {
    await seedImages(page, 1, {
      width: 800,
      height: 600,
      video,
      invalid: true,
    });
    const row = page.locator('[data-message-id="scroll-message-0"]');
    await expect(row.getByRole("status")).toBeVisible();
    const before = await row.evaluate(
      (element) => element.getBoundingClientRect().height,
    );
    await releaseImages(page);
    await expect(
      row.getByTestId(
        video ? "file-video-unsupported" : "file-image-unsupported",
      ),
    ).toBeVisible();
    expect(
      await row.evaluate((element) => element.getBoundingClientRect().height),
    ).toBe(before);
    await expect(row.getByTestId("media-fallback-save")).toBeEnabled();
  });
}
