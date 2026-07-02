import { test } from "./tauri-mock";
import { enterChat, BOB, CAROL } from "./helpers/session";
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
    `conversation-rename-${BOB.account}`,
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

test("stranded prompt dismiss target meets interaction invariants", async ({
  page,
}) => {
  await page.clock.install();
  await enterChat(page);

  await page.evaluate(
    ({ bob, carol }) => {
      const setPresence = (
        window as unknown as {
          __mockSetPresence?: (
            next: Record<
              string,
              { online: boolean; last_seen_secs: number | null }
            >,
          ) => void;
        }
      ).__mockSetPresence;
      if (!setPresence) throw new Error("__mockSetPresence is not installed");
      setPresence({
        [bob]: { online: false, last_seen_secs: 120 },
        [carol]: { online: false, last_seen_secs: 120 },
      });
    },
    { bob: BOB.account, carol: CAROL.account },
  );

  await page.clock.runFor(25_000);

  await expectMinTargetSize(
    page.getByTestId("stranded-dismiss"),
    32,
    "stranded-dismiss",
  );
  await expectVisibleFocus(page, page.getByTestId("stranded-dismiss"));
});
