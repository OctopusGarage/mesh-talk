import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, revealComposerTools } from "./helpers/session";

// Successful portable scenarios retain evidence in the full Chromium run.
test.use({ trace: "on" });

test.beforeEach(async ({ context, browserName }) => {
  if (browserName === "chromium")
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
});

async function openCapture(
  page: import("@playwright/test").Page,
  mode: "screenshot-now" | "screenshot-hidden",
) {
  await revealComposerTools(page);
  await page.getByTestId("composer-screenshot").click();
  await page.getByTestId(mode).click();
  await expect(page.getByTestId("screenshot-editor")).toBeVisible();
}

async function selectRegion(page: import("@playwright/test").Page) {
  const canvas = page.getByTestId("screenshot-canvas");
  await canvas.dragTo(canvas, {
    sourcePosition: { x: 10, y: 10 },
    targetPosition: { x: 80, y: 60 },
  });
  await expect(page.getByTestId("screenshot-send")).toBeEnabled();
}

test("Enter sends a staged screenshot after capture loses input focus", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  for (const mode of ["screenshot-now", "screenshot-hidden"] as const) {
    await openCapture(page, mode);
    await selectRegion(page);
    await page.getByTestId("screenshot-send").click();
    await expect(page.getByTestId("composer-pending-image")).toBeVisible();
    // Simulate native window restoration leaving keyboard focus on the background.
    await page.getByTestId("composer-input").evaluate((input) => input.blur());
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("composer-pending-image")).toHaveCount(0);
    await expect(
      page.getByRole("article", { name: /screenshot\.png/ }).last(),
    ).toBeVisible();
  }
});

test("screenshot selection annotation and explicit send produce cropped PNG", async ({
  page,
}, testInfo) => {
  await enterChat(page, "tester", "/?data=screenshot-light");
  await openBobDm(page);
  await openCapture(page, "screenshot-now");
  await expect(page.getByTestId("screenshot-canvas")).toBeVisible();
  await expect(page.getByTestId("screenshot-send")).toBeDisabled();
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Select a new area" }).click();
  await selectRegion(page);
  const canvas = page.getByTestId("screenshot-canvas");
  await page.getByRole("button", { name: "Draw", exact: true }).click();
  await canvas.dragTo(canvas, {
    sourcePosition: { x: 25, y: 25 },
    targetPosition: { x: 60, y: 45 },
  });
  const editorImage = testInfo.outputPath("editor.png");
  await page.screenshot({ path: editorImage });
  await testInfo.attach("screenshot-editor-before-send", {
    path: editorImage,
    contentType: "image/png",
  });
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "screenshot.png",
  );
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
  // Chromium permits automated clipboard reads. WebKit still exercises the
  // real gesture-triggered write above, without automated read permission.
  let copiedBytes: number[] | undefined;
  if (testInfo.project.name === "chromium") {
    copiedBytes = await page.evaluate(async () => {
      const [item] = await navigator.clipboard.read();
      const png = await item.getType("image/png");
      return Array.from(new Uint8Array(await png.arrayBuffer()));
    });
  }
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toBeVisible();
  const saved = await page.evaluate(() => {
    const bytes = (window as unknown as { __lastWrittenFileBytes?: number[] })
      .__lastWrittenFileBytes;
    if (!bytes) return null;
    const view = new DataView(new Uint8Array(bytes).buffer);
    return {
      signature: bytes.slice(0, 8),
      width: view.getUint32(16),
      height: view.getUint32(20),
    };
  });
  if (copiedBytes) {
    const samePixels = await page.evaluate(async (clipboardBytes) => {
      const sentBytes = (
        window as unknown as { __lastWrittenFileBytes: number[] }
      ).__lastWrittenFileBytes;
      async function pixels(bytes: number[]) {
        const image = await createImageBitmap(
          new Blob([new Uint8Array(bytes)], { type: "image/png" }),
        );
        const canvas = document.createElement("canvas");
        canvas.width = image.width;
        canvas.height = image.height;
        const context = canvas.getContext("2d")!;
        context.drawImage(image, 0, 0);
        const data = Array.from(
          context.getImageData(0, 0, image.width, image.height).data,
        );
        image.close();
        return { width: canvas.width, height: canvas.height, data };
      }
      return (
        JSON.stringify(await pixels(clipboardBytes)) ===
        JSON.stringify(await pixels(sentBytes))
      );
    }, copiedBytes);
    expect(samePixels).toBe(true);
  }
  expect(saved).toEqual({
    signature: [137, 80, 78, 71, 13, 10, 26, 10],
    width: 70,
    height: 50,
  });
});

test("hidden screenshot capture cancels without sending", async ({ page }) => {
  await enterChat(page);
  await openBobDm(page);
  await openCapture(page, "screenshot-hidden");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  await openCapture(page, "screenshot-hidden");
  await selectRegion(page);
  await page.getByRole("button", { name: "Cancel", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  await expect(page.getByTestId("composer-pending-files")).toHaveCount(0);
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
});

test("sender previews a screenshot when media IPC returns fallback byte arrays", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=screenshot-ipc-fallback");
  await openBobDm(page);
  await openCapture(page, "screenshot-now");
  await selectRegion(page);
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "screenshot.png",
  );
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
  await page.getByTestId("composer-send").click();

  const image = page.getByTestId("file-image");
  await expect(image).toBeVisible();
  await expect
    .poll(() =>
      image.evaluate((element) => (element as HTMLImageElement).naturalWidth),
    )
    .toBeGreaterThan(0);
  await expect(page.getByTestId("file-image-unsupported")).toHaveCount(0);
});

test("rectangle arrow text and undo remain usable before send", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await openCapture(page, "screenshot-now");
  await page.getByRole("button", { name: "Select a new area" }).click();
  await selectRegion(page);
  const canvas = page.getByTestId("screenshot-canvas");
  for (const [tool, start, end] of [
    ["Rectangle", { x: 20, y: 20 }, { x: 50, y: 40 }],
    ["Arrow", { x: 25, y: 45 }, { x: 55, y: 25 }],
  ] as const) {
    await page.getByRole("button", { name: tool }).click();
    await canvas.dragTo(canvas, { sourcePosition: start, targetPosition: end });
  }
  await page.getByRole("button", { name: "Text", exact: true }).click();
  await page.getByRole("textbox", { name: "Annotation text" }).fill("note");
  await canvas.click({ position: { x: 30, y: 35 } });
  const undo = page.getByRole("button", { name: "Undo annotation" });
  await expect(undo).toBeEnabled();
  for (let index = 0; index < 3; index += 1) await undo.click();
  await expect(undo).toBeDisabled();
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "screenshot.png",
  );
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
  await page.getByTestId("composer-send").click();
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toBeVisible();
});

test("unavailable capture is disabled and failed send preserves staged screenshot", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=screenshot-unavailable");
  await openBobDm(page);
  await revealComposerTools(page);
  await expect(page.getByTestId("composer-screenshot")).toBeDisabled();

  await page.goto("/?data=screenshot-write-fails");
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await openBobDm(page);
  await openCapture(page, "screenshot-now");
  await selectRegion(page);
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "screenshot.png",
  );
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
  await page.getByTestId("composer-send").click();
  await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "screenshot.png",
  );
  await expect(page.getByTestId("composer").getByRole("alert")).toBeVisible();
  await expect(
    page.getByRole("article", { name: /screenshot\.png/ }),
  ).toHaveCount(0);
});

for (const [width, height] of [
  [1280, 720],
  [760, 620],
] as const) {
  test(`desktop overlay at ${width}x${height} preserves Retina crop and keeps controls in bounds`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height });
    await enterChat(page, "tester", "/?data=screenshot-desktop");
    await openBobDm(page);
    await openCapture(page, "screenshot-hidden");
    const canvas = page.getByTestId("screenshot-canvas");
    await canvas.dragTo(canvas, {
      sourcePosition: { x: 100, y: 110 },
      targetPosition: { x: width - 40, y: height - 40 },
    });
    const dimensions = page.getByTestId("screenshot-dimensions");
    await expect(dimensions).toHaveText(
      `${(width - 140) * 2} × ${(height - 150) * 2}`,
    );
    // Drag the lower-right handle and keep image-pixel dimensions at 2x display scale.
    await canvas.dragTo(canvas, {
      sourcePosition: { x: width - 40, y: height - 40 },
      targetPosition: { x: width - 20, y: height - 20 },
    });
    await expect(dimensions).toHaveText(
      `${(width - 120) * 2} × ${(height - 130) * 2}`,
    );
    await page.getByRole("button", { name: "Ellipse", exact: true }).click();
    await canvas.dragTo(canvas, {
      sourcePosition: { x: 150, y: 240 },
      targetPosition: { x: 280, y: 320 },
    });
    await page.getByRole("button", { name: "Mosaic", exact: true }).click();
    await canvas.dragTo(canvas, {
      sourcePosition: { x: 330, y: 260 },
      targetPosition: { x: 430, y: 340 },
    });
    await page.getByRole("button", { name: "Text", exact: true }).click();
    await page
      .getByRole("textbox", { name: "Annotation text" })
      .fill("Review this area");
    await canvas.click({ position: { x: 150, y: 360 } });
    const toolbar = await page.getByTestId("screenshot-toolbar").boundingBox();
    expect(toolbar).not.toBeNull();
    expect(toolbar!.x).toBeGreaterThanOrEqual(0);
    expect(toolbar!.y).toBeGreaterThanOrEqual(0);
    expect(toolbar!.x + toolbar!.width).toBeLessThanOrEqual(width);
    expect(toolbar!.y + toolbar!.height).toBeLessThanOrEqual(height);
    const path = testInfo.outputPath("desktop-overlay.png");
    await page.screenshot({ path });
    await testInfo.attach("desktop-overlay", {
      path,
      contentType: "image/png",
    });
    await page.getByRole("button", { name: "Select a new area" }).click();
    await page.getByTestId("screenshot-editor").focus();
    await page.keyboard.press("Enter");
    await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
    await expect(page.getByTestId("composer-pending-image")).toBeVisible();
    await expect(
      page.getByRole("article", { name: /screenshot\.png/ }),
    ).toHaveCount(0);
    await page.getByTestId("composer-send").click();
    // Sending reads the staged File asynchronously before writing it through IPC.
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as unknown as { __lastWrittenFileBytes?: number[] })
              .__lastWrittenFileBytes?.length ?? 0,
        ),
      )
      .toBeGreaterThan(24);
    const saved = await page.evaluate(() => {
      const bytes = (window as unknown as { __lastWrittenFileBytes: number[] })
        .__lastWrittenFileBytes;
      const view = new DataView(new Uint8Array(bytes).buffer);
      return { width: view.getUint32(16), height: view.getUint32(20) };
    });
    expect(saved).toEqual({
      width: (width - 120) * 2,
      height: (height - 130) * 2,
    });
    const pixels = await page.evaluate(async () => {
      const bytes = (window as unknown as { __lastWrittenFileBytes: number[] })
        .__lastWrittenFileBytes;
      const url = URL.createObjectURL(
        new Blob([new Uint8Array(bytes)], { type: "image/png" }),
      );
      const image = new Image();
      image.src = url;
      await image.decode();
      const output = document.createElement("canvas");
      output.width = image.naturalWidth;
      output.height = image.naturalHeight;
      const ctx = output.getContext("2d")!;
      ctx.drawImage(image, 0, 0);
      URL.revokeObjectURL(url);
      const pixel = (x: number, y: number) =>
        Array.from(ctx.getImageData(x, y, 1, 1).data);
      return {
        ellipse: pixel(230, 260),
        mosaicStart: pixel(461, 301),
        mosaicSameBlock: pixel(470, 301),
        mosaicNextBlock: pixel(500, 301),
        corner: pixel(0, 0),
      };
    });
    expect(pixels.ellipse[0]).toBeGreaterThan(200);
    expect(pixels.ellipse[1]).toBeLessThan(130);
    expect(pixels.ellipse[2]).toBeLessThan(130);
    expect(pixels.mosaicStart).toEqual(pixels.mosaicSameBlock);
    expect(pixels.mosaicStart).not.toEqual(pixels.mosaicNextBlock);
    expect(pixels.corner).toEqual([255, 255, 255, 255]);
  });
}

test("clipboard denial retains the edited screenshot for retry without staging", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const write = navigator.clipboard.write.bind(navigator.clipboard);
    let denied = false;
    navigator.clipboard.write = (items) => {
      if (!denied) {
        denied = true;
        return Promise.reject(new Error("Clipboard permission denied"));
      }
      return write(items);
    };
  });
  await openCapture(page, "screenshot-now");
  await selectRegion(page);
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByRole("alert")).toContainText(
    "Clipboard permission denied",
  );
  await expect(page.getByTestId("composer-pending-files")).toHaveCount(0);
  await expect(page.getByTestId("screenshot-editor")).toBeVisible();
  await page.getByTestId("screenshot-send").click();
  await expect(page.getByTestId("composer-pending-image")).toBeVisible();
});
