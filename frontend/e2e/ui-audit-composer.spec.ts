import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm } from "./helpers/session";
import { prepareForScreenshot, snapshotName } from "./helpers/visual-snapshot";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 820, height: 620 } });

test("composer controls and popovers stay usable at narrow width", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.getByTestId("composer-input").fill("composer audit");
  await page.getByTestId("composer-more-tools").click();

  for (const id of [
    "composer-screenshot",
    "composer-attach",
    "composer-image",
    "composer-emoji",
    "composer-send",
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 36, id);
    await expectVisibleFocus(page, page.getByTestId(id));
  }

  await page.getByTestId("composer-emoji").click();
  await expect(page.getByTestId("composer-emoji")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="expression-picker"]', "body");
  await expectMinTargetSize(
    page.getByTestId("composer-stickers"),
    36,
    "sticker tab",
  );
  await page.getByTestId("composer-emoji-tab").focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await expect(page.getByTestId("composer-stickers")).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByTestId("emoji-picker")).toBeVisible();
  await expectVisibleFocus(page, page.getByTestId("emoji-option-😀"));
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("composer-emoji")).toHaveAttribute(
    "aria-expanded",
    "false",
  );

  await page.getByTestId("composer-emoji").click();
  await page.getByTestId("composer-stickers").click();
  await expect(page.getByTestId("sticker-panel")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="expression-picker"]', "body");
  await expectVisibleFocus(page, page.getByTestId("sticker-option-1f602"));
  await page.keyboard.press("Escape");

  await page.getByTestId("composer-screenshot").click();
  await expect(page.getByTestId("screenshot-menu")).toBeVisible();
  await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
  await expectVisibleFocus(page, page.getByTestId("screenshot-now"));
  await expectNoHorizontalOverflow(page, "composer popovers");
});

test.describe("composer visual baselines", () => {
  test.use({ viewport: { width: 1280, height: 800 } });

  test("composer toolbar and input row match the committed baseline", async ({
    page,
  }) => {
    await enterChat(page);
    await openBobDm(page);
    await page.getByTestId("composer-input").fill("composer audit");
    await prepareForScreenshot(page);

    await expect(page.getByTestId("composer")).toHaveScreenshot(
      snapshotName("composer-toolbar", "dark", 1280, 800),
    );
  });
});

test.describe("narrow composer pane", () => {
  test.use({ viewport: { width: 760, height: 620 } });

  test("composer popovers fit when a persisted wide sidebar narrows the chat pane", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      localStorage.setItem("mesh-talk-sidebar-width", "460");
    });
    await enterChat(page);
    await openBobDm(page);
    await prepareForScreenshot(page);
    await page.getByTestId("composer-more-tools").click();

    await page.getByTestId("composer-emoji").click();
    await expect(page.getByTestId("emoji-picker")).toBeVisible();
    await expectElementsWithin(
      page,
      '[data-testid="expression-picker"]',
      "body",
    );
    await expectNoHorizontalOverflow(page, "narrow pane emoji picker");
    await expect(page.getByTestId("expression-picker")).toHaveScreenshot(
      snapshotName("composer-emoji", "dark", 760, 620),
      {
        maxDiffPixelRatio: 0.2,
      },
    );
    await page.keyboard.press("Escape");

    await page.getByTestId("composer-emoji").click();
    await page.getByTestId("composer-stickers").click();
    await expect(page.getByTestId("sticker-panel")).toBeVisible();
    await expectElementsWithin(
      page,
      '[data-testid="expression-picker"]',
      "body",
    );
    await expectNoHorizontalOverflow(page, "narrow pane sticker panel");
    await expect(page.getByTestId("expression-picker")).toHaveScreenshot(
      snapshotName("composer-stickers", "dark", 760, 620),
    );
    await page.keyboard.press("Escape");

    await page.getByTestId("composer-screenshot").click();
    await expect(page.getByTestId("screenshot-menu")).toBeVisible();
    await expectElementsWithin(page, '[data-testid="screenshot-menu"]', "body");
    await expectNoHorizontalOverflow(page, "narrow pane screenshot menu");
    await expect(page.getByTestId("screenshot-menu")).toHaveScreenshot(
      snapshotName("composer-screenshot", "dark", 760, 620),
      {
        maxDiffPixelRatio: 0.08,
      },
    );
  });
});
