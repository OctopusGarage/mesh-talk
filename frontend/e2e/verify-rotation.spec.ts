import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";

test("a changed device never shows the previous safety number", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.getByTestId("verify-trigger").click();
  const dialog = page.getByTestId("verify-dialog");
  await expect(dialog.getByTestId("safety-number")).toBeVisible();

  await page.evaluate(() => {
    const mock = window as unknown as {
      __mockFailNext: (command: string) => void;
      __mockRotateBobDevice: () => void;
    };
    mock.__mockFailNext("safety_number");
    mock.__mockRotateBobDevice();
  });

  await expect(dialog.getByText("network unreachable")).toBeVisible();
  await expect(dialog.getByTestId("safety-number")).toHaveCount(0);
});
