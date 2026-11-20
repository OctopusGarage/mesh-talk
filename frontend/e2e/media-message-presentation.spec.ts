import { test, expect } from "./tauri-mock";
import {
  enterChat,
  openBobDm,
  revealComposerAttachments,
  seedThemeBeforeLoad,
} from "./helpers/session";

for (const scene of [
  {
    theme: "light",
    viewport: { width: 1100, height: 800 },
    width: 800,
    height: 400,
  },
  {
    theme: "dark",
    viewport: { width: 760, height: 700 },
    width: 400,
    height: 800,
  },
]) {
  test(`photo preparation stays quiet and preserves proportions (${scene.theme})`, async ({
    page,
  }, info) => {
    await page.setViewportSize(scene.viewport);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await seedThemeBeforeLoad(page, scene.theme);
    await enterChat(page);
    await openBobDm(page);
    await page.evaluate(async ({ width, height }) => {
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#3c7b70";
      context.fillRect(0, 0, width, height);
      context.fillStyle = "#ece1ba";
      context.fillRect(width / 4, height / 4, width / 2, height / 2);
      const blob = await new Promise<Blob>((resolve) =>
        canvas.toBlob((value) => resolve(value!), "image/png"),
      );
      const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
      const win = window as unknown as {
        __photo: { release?: () => void; originals: number };
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      win.__photo = { originals: 0 };
      const invoke = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = (cmd, args) => {
        if (cmd === "plugin:dialog|open")
          return Promise.resolve("/tmp/photo.png");
        if (cmd === "owner_enqueue_file")
          return new Promise((resolve, reject) => {
            win.__photo.release = () => {
              void invoke(cmd, args).then(resolve, reject);
            };
          });
        if (cmd === "read_media_preview") return Promise.resolve(bytes);
        if (cmd === "read_media") {
          win.__photo.originals++;
          return Promise.resolve(bytes);
        }
        return invoke(cmd, args);
      };
    }, scene);
    await revealComposerAttachments(page);
    await page.getByTestId("composer-image").click();
    const bubble = page.getByTestId("message-bubble").last();
    const pending = bubble.locator("[data-media-placeholder]");
    await expect(pending).toBeVisible();
    await expect(bubble).toHaveAccessibleName(/photo.*sending/i);
    await expect(bubble.getByRole("progressbar")).toHaveCount(0);
    await expect(bubble.getByText(/Waiting for file|\d+%/)).toHaveCount(0);
    const waitingBounds = await pending.boundingBox();
    expect(waitingBounds!.width).toBeLessThanOrEqual(192);
    expect(waitingBounds!.height).toBeLessThanOrEqual(144);
    await page.screenshot({
      path: info.outputPath(`photo-${scene.theme}-pending.png`),
    });
    await page.evaluate(() =>
      (
        window as unknown as { __photo: { release: () => void } }
      ).__photo.release(),
    );
    const photo = bubble.getByTestId("file-image");
    await expect(photo).toBeVisible();
    await expect(photo).toHaveCSS("opacity", "1");
    const bounds = await photo.boundingBox();
    expect(bounds!.width).toBeLessThanOrEqual(320);
    expect(bounds!.height).toBeLessThanOrEqual(320);
    expect(bounds!.width / bounds!.height).toBeCloseTo(
      scene.width / scene.height,
      1,
    );
    await expect(bubble.getByRole("progressbar")).toHaveCount(0);
    expect(
      await page.evaluate(
        () =>
          (window as unknown as { __photo: { originals: number } }).__photo
            .originals,
      ),
    ).toBe(0);
    await page.screenshot({
      path: info.outputPath(`photo-${scene.theme}-ready.png`),
    });
    await photo.click();
    await expect(page.getByTestId("file-image-lightbox")).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __photo: { originals: number } }).__photo
              .originals,
        ),
      )
      .toBeGreaterThan(0);
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("file-image-lightbox")).toBeHidden();
  });
}
