import { test } from "./tauri-mock";
import { enterChat, BOB } from "./helpers/session";
import {
  expectElementsWithin,
  expectMinTargetSize,
  expectNoHorizontalOverflow,
  expectVisibleFocus,
} from "./helpers/ui-audit";

test.use({ viewport: { width: 1100, height: 760 } });

test("shell and sidebar meet baseline layout and interaction invariants", async ({
  page,
}) => {
  await enterChat(page);

  await expectNoHorizontalOverflow(page, "chat shell");
  await expectElementsWithin(
    page,
    '[data-testid^="conversation-row-"]',
    '[data-testid="sidebar"]',
  );

  await page.getByTestId(`conversation-row-${BOB.account}`).hover();
  for (const id of [
    "open-profile",
    "sidebar-overflow",
    `conversation-pin-${BOB.account}`,
  ]) {
    await expectMinTargetSize(page.getByTestId(id), 32, id);
  }

  await expectVisibleFocus(page, page.getByTestId("sidebar-overflow"));
  await expectVisibleFocus(
    page,
    page.getByTestId(`conversation-row-${BOB.account}`),
  );
});
