import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`offline help finds an article and navigates safely at ${viewport.width}`, async ({
    page,
    context,
  }) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await page.evaluate(() => {
      const host = window as unknown as {
        __helpMutations: string[];
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      host.__helpMutations = [];
      const invoke = host.__TAURI_INTERNALS__.invoke;
      host.__TAURI_INTERNALS__.invoke = (command, args) => {
        if (
          command === "set_app_settings" ||
          command.startsWith("plugin:dialog|")
        )
          host.__helpMutations.push(command);
        return invoke(command, args);
      };
    });
    const externalRequests: string[] = [];
    await context.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (!["127.0.0.1", "localhost"].includes(url.hostname)) {
        externalRequests.push(url.href);
        await route.abort();
      } else await route.continue();
    });
    await openSidebarMenuAction(page, "sidebar-nav-help");
    await expectDialogFitsViewport(page, "help-dialog");
    const search = page.getByTestId("help-search");
    await search.fill("下载目录");
    await page.getByTestId("help-topic-download-location").click();
    await expect(page.getByTestId("help-article")).toContainText(
      "Download folder",
    );
    if (process.env.MESH_TALK_HELP_SCREENSHOTS === "1") {
      const directory = fileURLToPath(
        new URL("../../.impeccable/review/", import.meta.url),
      );
      await mkdir(directory, { recursive: true });
      await page.screenshot({
        path: `${directory}/help-app-${viewport.width}.png`,
        animations: "disabled",
      });
    }
    await page.getByTestId("help-open-target").click();
    await expect(page.getByTestId("help-dialog")).toBeHidden();
    await expect(page.getByTestId("settings-page-files")).toBeVisible();
    await expect(page.locator("#setting-download-folder")).toBeFocused();
    await expect(page.getByTestId("settings-dialog")).toContainText(
      "Download folder",
    );
    await page.keyboard.press("Escape");
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    await expect(page.getByTestId("settings-page-general")).toBeVisible();
    expect(externalRequests).toEqual([]);
    expect(
      await page.evaluate(
        () =>
          (window as unknown as { __helpMutations: string[] }).__helpMutations,
      ),
    ).toEqual([]);
  });
}

test("signed-out help supports keyboard search, article links and empty results", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-help").click();
  const search = page.getByTestId("help-search");
  await expect(search).toBeFocused();
  await search.fill("restore");
  await search.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("help-article")).toContainText("recovery key");
  await expect(page.getByTestId("help-open-target")).toHaveCount(0);
  await search.fill("zzzz-no-such-help-123");
  await expect(page.getByTestId("help-no-results")).toBeVisible();
  await page.getByRole("button", { name: "Clear search", exact: true }).click();
  await page.getByTestId("help-topic-first-run").click();
  await page
    .getByTestId("help-article")
    .getByRole("link", { name: "encrypted backup", exact: true })
    .click();
  await expect(
    page.getByTestId("help-article").getByRole("heading", { level: 1 }),
  ).toHaveText("Back up and restore your account");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("login-help")).toBeFocused();
});

test("signed-out tray Help opens the offline reader", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("login-help")).toBeVisible();
  await page.evaluate(() => {
    (
      window as unknown as {
        __mockEmit: (event: string, payload: unknown) => void;
      }
    ).__mockEmit("tray-navigate", "help");
  });
  await expect(page.getByTestId("help-dialog")).toBeVisible();
  await expect(page.getByTestId("help-search")).toBeFocused();
  await expect(page.getByTestId("help-open-target")).toHaveCount(0);
});
