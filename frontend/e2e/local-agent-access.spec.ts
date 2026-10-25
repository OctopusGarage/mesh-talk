import { test, expect } from "./tauri-mock";
import { enterChat } from "./helpers/session";
import { openSidebarMenuAction } from "./helpers/sidebar-actions";
import { expectDialogFitsViewport } from "./helpers/ui-audit";
import type { Page } from "@playwright/test";

async function mockAccess(page: Page, patch: Record<string, unknown> = {}) {
  return page.evaluate((value) => {
    const update = (
      window as unknown as {
        __mockSetLocalAgentAccess: (patch: Record<string, unknown>) => {
          lifetime: string;
          expires_at_ms: number | null;
        };
      }
    ).__mockSetLocalAgentAccess;
    return update(value);
  }, patch);
}

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
    const panel = page.getByTestId("local-agent-settings");
    const toggle = panel.locator("#setting-local-agent-access");
    await expect(toggle).toBeDisabled();
    await panel.getByRole("checkbox", { name: /bob/i }).check();
    await expect(toggle).toBeEnabled();
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "true");
    await expect(panel.getByRole("status")).toContainText("Running");
    await panel.locator("summary").click();
    const instructions = await panel.locator("pre").innerText();
    expect(instructions).toContain("/v1/help");
    expect(instructions).not.toContain("agent-access.json");
    await panel.getByTestId("copy-agent-instructions").click();
    await expect
      .poll(async () => {
        const copied = await page.evaluate(() =>
          navigator.clipboard.readText(),
        );
        // Windows clipboard reads use CRLF; preserve every other prompt byte.
        return copied.replace(/\r\n/g, "\n");
      })
      .toBe(instructions);
    await expectDialogFitsViewport(page, "settings-dialog");
    await page.screenshot({
      path: testInfo.outputPath("local-agent-settings.png"),
    });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-checked", "false");
    await expect(panel.locator("pre")).toHaveCount(0);
    await toggle.click();
    await expect(panel.locator("summary")).toHaveCount(1);
  });
}

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
  const panel = page.getByTestId("local-agent-settings");
  await expect(panel.getByRole("checkbox", { name: /bob/i })).toHaveCount(0);
  await panel.getByRole("checkbox", { name: /team/i }).check();
  await panel.locator("#setting-local-agent-access").click();
  await expect(panel.getByRole("status")).toContainText("Running");
});

test("temporary access preserves its deadline across refresh and scope edits", async ({
  page,
}) => {
  await enterChat(page);
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const panel = page.getByTestId("local-agent-settings");
  await expect(panel.locator("#agent-access-duration")).toHaveValue("one_hour");
  await panel.getByRole("checkbox", { name: /bob/i }).check();
  await panel.locator("#setting-local-agent-access").click();
  const initial = await mockAccess(page);
  expect(initial.expires_at_ms).not.toBeNull();
  await panel.getByTestId("refresh-agent-activity").click();
  await expect(panel.getByRole("status")).toContainText("Running");
  expect((await mockAccess(page)).expires_at_ms).toBe(initial.expires_at_ms);
  // A draft continuous choice does not silently expand authority on a scope edit.
  await panel.locator("#agent-access-duration").selectOption("persistent");
  await panel.getByRole("checkbox", { name: /team/i }).check();
  expect((await mockAccess(page)).expires_at_ms).toBe(initial.expires_at_ms);
  await panel.getByTestId("renew-agent-access").click();
  await expect
    .poll(async () => (await mockAccess(page)).lifetime)
    .toBe("persistent");
  expect((await mockAccess(page)).expires_at_ms).toBeNull();
});

test("expired authorization stays revocable and activity renders only safe metadata", async ({
  page,
}) => {
  await enterChat(page);
  await mockAccess(page, {
    enabled: true,
    running: true,
    lifetime: "one_hour",
    expires_at_ms: Date.now() - 1000,
    destinations: [{ kind: "account", id: "acc_bob_bbbb2222" }],
    recent_activity: [
      {
        at_ms: Date.now() - 2000,
        destination: { kind: "account", id: "acc_bob_bbbb2222" },
        action: "accepted",
        text: "private body must not appear",
        token: "private credential must not appear",
        operation_id: "raw retry identifier must not appear",
      },
    ],
  });
  await openSidebarMenuAction(page, "sidebar-nav-settings");
  const panel = page.getByTestId("local-agent-settings");
  await expect(panel.getByRole("status")).toContainText("Expired");
  await expect(panel.locator("pre")).toHaveCount(0);
  const activity = panel.getByTestId("agent-access-activity");
  await expect(activity).toContainText("Local agent interface");
  await expect(activity).toContainText("Accepted");
  await expect(activity).not.toContainText("private body");
  await expect(activity).not.toContainText("private credential");
  await expect(activity).not.toContainText("raw retry");
  const revoke = panel.locator("#setting-local-agent-access");
  await expect(revoke).toBeEnabled();
  await revoke.click();
  await expect(revoke).toHaveAttribute("aria-checked", "false");
});
