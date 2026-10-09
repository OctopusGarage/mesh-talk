import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";

test.use({ hasTouch: true, viewport: { width: 760, height: 520 } });

test("frequent icon controls keep a 44px touch target", async ({ page }) => {
  await enterChat(page);
  expect(await page.evaluate(() => matchMedia("(hover: none)").matches)).toBe(
    true,
  );
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();

  for (const testId of [
    "composer-more-tools",
    "composer-send",
    "conversation-actions-acc_bob_bbbb2222",
  ]) {
    const box = await page.getByTestId(testId).boundingBox();
    expect(box, testId).not.toBeNull();
    expect(box!.width, testId).toBeGreaterThanOrEqual(44);
    expect(box!.height, testId).toBeGreaterThanOrEqual(44);
  }
  await page.getByTestId("composer-more-tools").click();
  const emoji = await page.getByTestId("composer-emoji").boundingBox();
  expect(emoji?.width).toBeGreaterThanOrEqual(44);
  expect(emoji?.height).toBeGreaterThanOrEqual(44);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);

  await page.getByTestId("message-bubble").first().hover();
  await page.getByTestId("message-reply").first().click();
  const cancelReply = await page
    .getByRole("button", { name: "Cancel reply" })
    .boundingBox();
  expect(cancelReply?.width).toBeGreaterThanOrEqual(44);
  expect(cancelReply?.height).toBeGreaterThanOrEqual(44);

  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("file-received", {
      from: "device_bob_2222",
      name: "report.pdf",
      size: 5678,
      file_conv: "touch_target_file",
      conv: "acc_bob_bbbb2222",
      mime: "application/pdf",
      media: false,
    });
  });
  await page.getByTestId("sidebar-action-files").click();
  const dismiss = page.getByTestId("files-tray").getByRole("button", {
    name: "Dismiss",
  });
  await expect(dismiss).toBeVisible();
  const box = await dismiss.boundingBox();
  expect(box?.width).toBeGreaterThanOrEqual(44);
  expect(box?.height).toBeGreaterThanOrEqual(44);
});

test("settings switches keep a compact track inside a usable touch target", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const wallpaper = page.getByTestId("settings-wallpaper");
  const bounds = await wallpaper.boundingBox();
  expect(bounds?.width).toBeGreaterThanOrEqual(44);
  expect(bounds?.height).toBeGreaterThanOrEqual(44);
  const before = await wallpaper.getAttribute("aria-checked");
  await wallpaper.tap();
  await expect(wallpaper).toHaveAttribute(
    "aria-checked",
    before === "true" ? "false" : "true",
  );
  await wallpaper.focus();
  await page.keyboard.press("Space");
  await expect(wallpaper).toHaveAttribute("aria-checked", before ?? "false");
});

test("form inputs and selects keep 44px touch targets", async ({ page }) => {
  await enterChat(page);

  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const settings = page.getByRole("dialog");
  const language = page.getByTestId("settings-language-select");
  expect(
    Math.round((await language.boundingBox())!.height),
  ).toBeGreaterThanOrEqual(44);
  await settings.getByRole("button", { name: "Close" }).click();

  await page.getByRole("button", { name: "New channel" }).click();
  const channel = page.getByTestId("create-channel-dialog");
  const name = channel.getByRole("textbox", { name: "Channel name" });
  expect(Math.round((await name.boundingBox())!.height)).toBeGreaterThanOrEqual(
    44,
  );
  await name.tap();
  await name.fill("Touch test");
  await expect(name).toHaveValue("Touch test");
  await channel.getByRole("button", { name: "Cancel" }).click();

  const linkAction = page.getByTestId("sidebar-action-link");
  if (!(await linkAction.isVisible())) {
    await page.getByTestId("sidebar-overflow").click();
  }
  await linkAction.click();
  const link = page.getByTestId("link-device-dialog");
  for (const control of [
    link.getByRole("combobox", { name: "Device" }),
    link.getByRole("textbox", { name: "pairing code" }),
  ]) {
    expect(
      Math.round((await control.boundingBox())!.height),
    ).toBeGreaterThanOrEqual(44);
  }
});
