import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, revealComposerTools } from "./helpers/session";

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

test("screenshot selection annotation and explicit send produce cropped PNG", async ({
  page,
}, testInfo) => {
  await enterChat(page);
  await openBobDm(page);
  await openCapture(page, "screenshot-now");
  // The macOS native selector returns an already selected region; Windows and
  // Linux receive a full-screen capture that requires a drag in the editor.
  if (
    await page.evaluate(() => /Mac OS X|Macintosh/i.test(navigator.userAgent))
  )
    await expect(page.getByTestId("screenshot-send")).toBeEnabled();
  else await expect(page.getByTestId("screenshot-send")).toBeDisabled();
  await expect(page.getByRole("article", { name: /pasted\.png/ })).toHaveCount(
    0,
  );
  await page.getByRole("button", { name: "Select a new area" }).click();
  await selectRegion(page);
  const canvas = page.getByTestId("screenshot-canvas");
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
  await expect(page.getByTestId("screenshot-editor")).toHaveCount(0);
  await expect(
    page.getByRole("article", { name: /pasted\.png/ }),
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
  await expect(page.getByRole("article", { name: /pasted\.png/ })).toHaveCount(
    0,
  );
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
  await expect(
    page.getByRole("article", { name: /pasted\.png/ }),
  ).toBeVisible();
});

test("unavailable capture is disabled and failed save preserves the editor", async ({
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
  await expect(
    page.getByTestId("screenshot-editor").getByRole("alert"),
  ).toContainText("disk full");
  await expect(page.getByTestId("screenshot-editor")).toBeVisible();
  await expect(page.getByRole("article", { name: /pasted\.png/ })).toHaveCount(
    0,
  );
});
