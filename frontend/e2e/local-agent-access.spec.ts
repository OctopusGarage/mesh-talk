import { test, expect } from "./tauri-mock";
import { selectSettingsCategory } from "./helpers/settings";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expectDialogFitsViewport } from "./helpers/ui-audit";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 760, height: 520 },
]) {
  test(`local agent settings authorize, disclose and copy at ${viewport.width}x${viewport.height}`, async ({
    page,
    context,
  }, testInfo) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.setViewportSize(viewport);
    await enterChat(page);
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    await expect(page.getByTestId("local-agent-settings")).toHaveCount(0);
    await selectSettingsCategory(page, "integrations");
    const panel = page.getByTestId("local-agent-settings");
    const toggle = panel.locator("#setting-local-agent-access");
    await expect(toggle).toBeDisabled();
    await panel
      .getByTestId("agent-conversation-picker")
      .locator("summary")
      .click();
    await panel.getByRole("checkbox", { name: /bob/i }).check();
    await expect(toggle).toBeEnabled();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByRole("status")).toContainText("Running");
    await expect(panel.getByTestId("copy-agent-instructions")).toBeVisible();
    await panel.getByTestId("copy-agent-instructions").click();
    await expect(panel.getByTestId("copy-agent-instructions")).toContainText(
      "Copied",
    );
    await panel.getByTestId("agent-instructions").locator("summary").click();
    const instructions = await panel.locator("pre").innerText();
    expect(instructions).toContain("/v1/help");
    expect(instructions).not.toContain("agent-access.json");
    await panel.getByTestId("copy-agent-instructions").click();
    await expect
      .poll(async () =>
        (await page.evaluate(() => navigator.clipboard.readText())).replace(
          /\r\n/g,
          "\n",
        ),
      )
      .toBe(instructions);
    await expectDialogFitsViewport(page, "settings-dialog");
    await page.screenshot({
      path: testInfo.outputPath("local-agent-settings.png"),
    });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await expect(panel.locator("pre")).toHaveCount(0);
    await toggle.click();
    await expect(
      panel.getByTestId("agent-instructions").locator("summary"),
    ).toHaveCount(1);
  });
}

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 390, height: 680 },
]) {
  test(`large agent conversation picker stays bounded at ${viewport.width}x${viewport.height}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize(viewport);
    await page.addInitScript(
      (theme) => localStorage.setItem("mesh-talk-theme", theme),
      viewport.width === 1440 ? "light" : "dark",
    );
    await enterChat(page);
    await page.evaluate(() => {
      const runtime = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              command: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const invoke = runtime.invoke.bind(runtime);
      runtime.invoke = async (command, args) => {
        const result = await invoke(command, args);
        if (command === "get_local_agent_access") {
          (
            result as { available_destinations: unknown[] }
          ).available_destinations = Array.from(
            { length: 300 },
            (_, index) => ({
              destination: {
                kind: index % 2 ? "channel" : "account",
                id: String(index),
              },
              label:
                index === 299
                  ? "喵星球"
                  : `Contact ${index} ${"LongName".repeat(15)}`,
            }),
          );
        }
        return result;
      };
    });
    await openSidebarMenuAction(page, "sidebar-nav-settings");
    await expect(page.getByTestId("local-agent-settings")).toHaveCount(0);
    await selectSettingsCategory(page, "integrations");
    const picker = page.getByTestId("agent-conversation-picker");
    const summary = picker.locator("summary");
    await expect(summary).toContainText("0 / 16 selected");
    await expect(picker.getByRole("searchbox")).toBeHidden();
    await summary.focus();
    await page.keyboard.press("Enter");
    const list = picker.getByTestId("agent-conversation-list");
    await expect(list).toBeVisible();
    const dimensions = await list.evaluate((element) => ({
      height: element.clientHeight,
      content: element.scrollHeight,
      width: element.clientWidth,
      contentWidth: element.scrollWidth,
    }));
    expect(dimensions.height).toBeLessThanOrEqual(224);
    expect(dimensions.content).toBeGreaterThan(dimensions.height);
    expect(dimensions.contentWidth).toBeLessThanOrEqual(dimensions.width);
    const search = picker.getByRole("searchbox");
    await search.fill("  喵星球  ");
    await expect(picker.getByRole("checkbox")).toHaveCount(1);
    await picker.getByRole("checkbox").check();
    await expect(summary).toContainText("1 / 16 selected");
    await search.fill("does not exist");
    await expect(picker.getByText("No matching conversations.")).toBeVisible();
    await search.fill("");
    await picker.getByRole("button", { name: "Selected only" }).click();
    await expect(picker.getByRole("checkbox")).toHaveCount(1);
    await expect(picker.getByRole("checkbox")).toBeChecked();
    await expectDialogFitsViewport(page, "settings-dialog");
    await page.screenshot({
      path: testInfo.outputPath("agent-picker-expanded.png"),
    });
    await summary.click();
    await expect(picker.getByRole("searchbox")).toBeHidden();
    await expect(summary).toContainText("1 / 16 selected");
    await page.screenshot({
      path: testInfo.outputPath("agent-picker-collapsed.png"),
    });
  });
}

test("integration settings explain a pending save and recover a failed scope change", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "integrations");
  const panel = page.getByTestId("local-agent-settings");
  await panel
    .getByTestId("agent-conversation-picker")
    .locator("summary")
    .click();
  await page.evaluate(() => {
    const win = window as unknown as {
      __releaseAgentSave: () => void;
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const original = win.__TAURI_INTERNALS__.invoke.bind(
      win.__TAURI_INTERNALS__,
    );
    win.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "set_local_agent_access") {
        await new Promise<void>((resolve) => {
          win.__releaseAgentSave = resolve;
        });
      }
      return original(command, args);
    };
  });
  await panel.getByRole("checkbox", { name: /bob/i }).click();
  await expect(panel.getByTestId("agent-access-status")).toContainText(
    "Saving",
  );
  await expect(panel.locator("#setting-local-agent-access")).toBeDisabled();
  await page.evaluate(() =>
    (
      window as unknown as { __releaseAgentSave: () => void }
    ).__releaseAgentSave(),
  );
  await expect(panel.getByRole("checkbox", { name: /bob/i })).toBeChecked();
  await expect(panel.getByTestId("agent-access-status")).toHaveText("Off");
  await page.evaluate(() =>
    (
      window as unknown as { __mockFailNext: (command: string) => void }
    ).__mockFailNext("set_local_agent_access"),
  );
  await panel.locator("#setting-local-agent-access").click();
  await page.evaluate(() =>
    (
      window as unknown as { __releaseAgentSave: () => void }
    ).__releaseAgentSave(),
  );
  await expect(panel.getByRole("alert")).toContainText("network unreachable");
  await expect(panel.locator("#setting-local-agent-access")).toHaveAttribute(
    "aria-checked",
    "false",
  );
  await expect(page.getByTestId("settings-page-integrations")).toBeVisible();
  await panel.getByTestId("retry-agent-access").click();
  await expect(panel.getByRole("alert")).toHaveCount(0);
  await expect(panel.getByRole("checkbox", { name: /bob/i })).toBeChecked();
});

test("hidden contacts cannot receive a new agent scope while groups remain available", async ({
  page,
}) => {
  await enterChat(page);
  const bob = "acc_bob_bbbb2222";
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await page.getByTestId(`hide-contact-menu-${bob}`).click();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  await selectSettingsCategory(page, "integrations");
  const panel = page.getByTestId("local-agent-settings");
  await panel
    .getByTestId("agent-conversation-picker")
    .locator("summary")
    .click();
  await expect(panel.getByRole("checkbox", { name: /bob/i })).toHaveCount(0);
  await panel.getByRole("checkbox", { name: /team/i }).check();
  await panel.locator("#setting-local-agent-access").click();
  await expect(panel.getByRole("status")).toContainText("Running");
});
