import { expect, test } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test("login settings wait for the native session to finish signing out", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const scope = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args?: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __logoutPending: boolean;
      __finishLogout: () => void;
      __ownerlessDuringLogout: number;
    };
    const invoke = scope.__TAURI_INTERNALS__.invoke.bind(
      scope.__TAURI_INTERNALS__,
    );
    scope.__logoutPending = false;
    scope.__ownerlessDuringLogout = 0;
    scope.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      if (cmd === "logout") {
        scope.__logoutPending = true;
        await new Promise<void>((resolve) => {
          scope.__finishLogout = resolve;
        });
        scope.__logoutPending = false;
      }
      if (cmd === "get_app_settings" && scope.__logoutPending && !args.owner) {
        scope.__ownerlessDuringLogout++;
        throw new Error("settings owner required");
      }
      return invoke(cmd, args);
    };
  });
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-sign-out").click();
  await expect(page.getByTestId("chat-shell")).toBeHidden();
  await expect(page.getByTestId("login-form")).toBeHidden();
  await page.evaluate(() =>
    (window as unknown as { __finishLogout: () => void }).__finishLogout(),
  );
  await expect(page.getByTestId("login-form")).toBeVisible();
  await expect(page.getByTestId("login-stay-signed-in")).toBeEnabled();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { __ownerlessDuringLogout: number })
          .__ownerlessDuringLogout,
    ),
  ).toBe(0);
});

test("a failed stay-signed-in change keeps its saved value", async ({
  page,
}) => {
  await page.goto("/");
  const stay = page.getByTestId("login-stay-signed-in");
  await expect(stay).toBeEnabled();
  await expect(stay).toBeChecked();
  await page.evaluate(() => {
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("set_app_settings");
  });
  await stay.click();
  await expect(page.getByRole("alert")).toContainText(
    "Couldn’t save the change",
  );
  await expect(stay).toBeChecked();

  await page.getByRole("button", { name: "Dismiss" }).click();
  await expect(stay).toBeEnabled();
  await stay.uncheck();
  await expect(stay).not.toBeChecked();
});
