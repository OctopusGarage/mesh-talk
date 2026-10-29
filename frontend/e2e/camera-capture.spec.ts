import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, revealComposerTools } from "./helpers/session";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`camera review and explicit attachment confirmation at ${viewport.width}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await page.addInitScript(() => {
      const original = navigator.mediaDevices;
      Object.defineProperty(navigator, "mediaDevices", {
        configurable: true,
        value: {
          getUserMedia: async () => {
            const canvas = document.createElement("canvas");
            canvas.width = 640;
            canvas.height = 480;
            const ctx = canvas.getContext("2d")!;
            ctx.fillStyle = "#375e76";
            ctx.fillRect(0, 0, 640, 480);
            return canvas.captureStream(10);
          },
          enumerateDevices: () => original.enumerateDevices(),
        },
      });
    });
    await enterChat(page);
    await openBobDm(page);
    await revealComposerTools(page);
    await page.getByTestId("composer-input").fill("Photo draft");
    await page.getByTestId("composer-camera").click();
    await expect(page.getByTestId("camera-shutter")).toBeEnabled();
    await page.screenshot({ path: testInfo.outputPath("camera-preview.png") });
    await page.getByTestId("camera-shutter").click();
    await expect(page.getByTestId("camera-photo")).toBeVisible();
    await expect(page.getByTestId("composer-pending-image")).toHaveCount(0);
    await page.getByTestId("camera-retake").click();
    await expect(page.getByTestId("camera-shutter")).toBeEnabled();
    await page.getByTestId("camera-shutter").click();
    await expect(page.getByTestId("camera-photo")).toBeVisible();
    await page.screenshot({
      path: testInfo.outputPath("camera-review.png"),
      animations: "disabled",
    });
    await page.getByTestId("camera-use-photo").click();
    await expect(page.getByTestId("camera-dialog")).toHaveCount(0);
    await expect(page.getByTestId("composer-pending-image")).toBeVisible();
    await expect(page.getByTestId("composer-input")).toHaveValue("Photo draft");
    await expect(page.getByTestId("composer-input")).toBeFocused();
  });
}
