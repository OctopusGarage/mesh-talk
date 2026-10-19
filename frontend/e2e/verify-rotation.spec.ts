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
      __mockRotateBobDevice: () => void;
      __safetyReadReady: Promise<void>;
      __failSafetyRead: () => void;
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const invoke = mock.__TAURI_INTERNALS__.invoke.bind(
      mock.__TAURI_INTERNALS__,
    );
    let ready!: () => void;
    mock.__safetyReadReady = new Promise((resolve) => {
      ready = resolve;
    });
    mock.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === "safety_number") {
        return new Promise((_resolve, reject) => {
          mock.__failSafetyRead = () =>
            reject(new Error("network unreachable"));
          ready();
        });
      }
      return invoke(command, args);
    };
    mock.__mockRotateBobDevice();
  });

  // Inspect the pending state before releasing the failure. This proves the
  // old device's number is hidden throughout the replacement request.
  await page.evaluate(
    () =>
      (window as unknown as { __safetyReadReady: Promise<void> })
        .__safetyReadReady,
  );
  await expect(dialog.getByTestId("safety-number")).toHaveCount(0);
  await page.evaluate(() =>
    (window as unknown as { __failSafetyRead: () => void }).__failSafetyRead(),
  );
  await expect(dialog.getByText("network unreachable")).toBeVisible();
  await expect(dialog.getByTestId("safety-number")).toHaveCount(0);
});
