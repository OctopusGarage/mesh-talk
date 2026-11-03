import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm } from "./helpers/session";

test("pasted image reserves its geometry before image decoding", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/pending-preview.svg", async (route) => {
    await gate;
    await route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="400" height="100"><rect width="400" height="100" fill="red"/></svg>',
    });
  });
  await page.evaluate(() => {
    URL.createObjectURL = () => `${location.origin}/pending-preview.svg`;
    URL.revokeObjectURL = () => {};
    const transfer = new DataTransfer();
    transfer.items.add(
      new File(["image"], "wide.svg", { type: "image/svg+xml" }),
    );
    document
      .querySelector('[data-testid="composer-input"]')!
      .dispatchEvent(
        new ClipboardEvent("paste", {
          clipboardData: transfer,
          bubbles: true,
          cancelable: true,
        }),
      );
  });
  const image = page.getByTestId("composer-pending-image");
  await expect(image).toBeAttached();
  const before = await image.boundingBox();
  release();
  await expect
    .poll(() =>
      image.evaluate((element) => (element as HTMLImageElement).naturalWidth),
    )
    .toBe(400);
  const after = await image.boundingBox();
  console.log({ before, after });
  expect(before).toEqual(after);
});
