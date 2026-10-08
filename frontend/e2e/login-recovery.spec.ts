import { expect, test } from "./tauri-mock";

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
