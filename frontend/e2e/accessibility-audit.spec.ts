import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";
import { test, expect } from "./tauri-mock";
import {
  enterChat,
  openBobDm,
  seedThemeBeforeLoad,
  CHANNEL,
} from "./helpers/session";

test.use({ reducedMotion: "reduce" });

async function expectNoWcagViolations(page: Page) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(
    results.violations.map((violation) => ({
      id: violation.id,
      impact: violation.impact,
      nodes: violation.nodes.map((node) => ({
        target: node.target,
        summary: node.failureSummary,
      })),
    })),
  ).toEqual([]);
}

test("login screen has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("login-submit")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("conversation has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await expectNoWcagViolations(page);
});

test("settings dialog has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("sidebar-nav-settings").click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("settings controls stay legible when privacy cannot load", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    ipc.invoke = (command, args) =>
      command === "get_privacy"
        ? Promise.reject(new Error("privacy unavailable"))
        : original(command, args);
  });
  await enterChat(page);
  await page.getByTestId("sidebar-nav-settings").click();
  const managePrivacy = page.getByTestId("manage-privacy");
  await expect(managePrivacy).toBeDisabled();
  expect(
    await managePrivacy.evaluate((button) =>
      Number(getComputedStyle(button).opacity),
    ),
  ).toBeGreaterThanOrEqual(0.7);
  await expectNoWcagViolations(page);
});

test("members dialog has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expectNoWcagViolations(page);
});

for (const theme of ["light", "oled", "argentina", "barcelona", "messi"]) {
  test(`conversation in ${theme} has no detectable WCAG A/AA violations`, async ({
    page,
  }) => {
    await seedThemeBeforeLoad(page, theme);
    await enterChat(page);
    await openBobDm(page);
    await expectNoWcagViolations(page);
  });
}

test("search dialog has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("sidebar-action-search").click();
  await expect(page.getByTestId("search-dialog")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("new channel dialog has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByRole("button", { name: "New channel" }).click();
  await expect(page.getByTestId("create-channel-dialog")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("connection dialog has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId("sidebar-nav-connection").click();
  await expect(page.getByTestId("diagnostics-dialog")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("empty shell at the minimum desktop size has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await page.goto("/?data=empty");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await expect(page.getByTestId("chat-shell")).toBeVisible();
  await expectNoWcagViolations(page);
});

test("members dialog at the minimum desktop size has no detectable WCAG A/AA violations", async ({
  page,
}) => {
  await page.setViewportSize({ width: 760, height: 520 });
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await expectNoWcagViolations(page);
});
