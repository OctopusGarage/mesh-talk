import { test, expect } from "./tauri-mock";
import type { Page } from "@playwright/test";
const bob = "acc_bob_bbbb2222";
async function hideBob(page: Page) {
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await page.getByTestId(`hide-contact-menu-${bob}`).click();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
}
async function login(page: Page, username = "tester") {
  await page.goto("/");
  await page.getByTestId("login-username").fill(username);
  await page.getByTestId("login-password").fill("a strong password");
  await page.getByTestId("login-submit").click();
  await expect(page.getByTestId("chat-shell")).toBeVisible();
}
async function manage(page: Page) {
  await page.getByTestId("sidebar-overflow").click();
  await page.getByTestId("sidebar-action-settings").click();
  await page.getByTestId("manage-hidden-contacts").click();
  await expect(page.getByTestId("hidden-contacts-dialog")).toBeVisible();
}
test("hide requires confirmation, persists across restart and restores history", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId(`conversation-row-${bob}`).click();
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await page.getByTestId(`hide-contact-menu-${bob}`).click();
  await page.getByTestId("hide-contact-cancel").click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeVisible();
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await page.getByTestId(`hide-contact-menu-${bob}`).click();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
  await expect(
    page.getByTestId("conversation-row-chan_team_dddd4444"),
  ).toBeVisible();
  await page.reload();
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("a strong password");
  await page.getByTestId("login-submit").click();
  await expect(
    page.getByTestId("conversation-row-acc_carol_cccc3333"),
  ).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
  await manage(page);
  await page.getByTestId(`restore-contact-${bob}`).click();
  await expect(page.getByTestId(`restore-contact-${bob}`)).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("hidden-contacts-dialog")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-dialog")).toHaveCount(0);
  await page.getByTestId(`conversation-row-${bob}`).click();
  await expect(
    page.getByText("hey, welcome to the mesh", { exact: true }),
  ).toBeVisible();
});
test("settings offers searchable keyboard-accessible hide and restore", async ({
  page,
}) => {
  await login(page);
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeVisible();
  await manage(page);
  await page.getByTestId("hidden-contacts-add-tab").click();
  await page.getByTestId("hidden-contacts-search").fill("bob");
  await page.getByTestId(`hide-contact-${bob}`).click();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(page.getByTestId(`hide-contact-${bob}`)).toHaveCount(0);
  await page.getByTestId("hidden-contacts-hidden-tab").click();
  await page.getByTestId(`restore-contact-${bob}`).click();
  await expect(page.getByTestId(`restore-contact-${bob}`)).toHaveCount(0);
});

test("failed save retains the contact and confirmation supports retry", async ({
  page,
}) => {
  await login(page);
  await page.evaluate(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    let fail = true;
    ipc.invoke = async (cmd, args) => {
      if (cmd === "set_contact_hidden" && fail) {
        fail = false;
        throw new Error("disk full");
      }
      return original(cmd, args);
    };
  });
  await page.getByTestId(`conversation-row-${bob}`).click({ button: "right" });
  await page.getByTestId(`hide-contact-menu-${bob}`).click();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(
    page.getByTestId("hide-contact-dialog").getByRole("alert"),
  ).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeAttached();
  await page.getByTestId("hide-contact-confirm").click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
});

test("hidden DMs are omitted from search, groups and inbound history remain intact", async ({
  page,
}) => {
  await login(page);
  await hideBob(page);
  await page.getByTestId("sidebar-action-search").click();
  await page.getByTestId("search-input").fill("welcome");
  await expect(page.getByText("No matches.", { exact: true })).toBeVisible();
  await expect(page.getByTestId("search-result")).toHaveCount(0);
  await page.getByTestId("search-input").fill("kickoff");
  await expect(page.getByTestId("search-result")).toHaveCount(1);
  await page.getByTestId("search-result").click();
  await expect(
    page.getByText("channel kickoff", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    const w = window as unknown as {
      __mockInject: (conv: string, text: string, who: string) => void;
      __mockEmit: (event: string, payload: unknown) => void;
    };
    w.__mockInject(
      "acc:acc_bob_bbbb2222",
      "received while hidden",
      "device_bob_2222",
    );
    w.__mockEmit("dm-received", {
      from: "device_bob_2222",
      from_name: "bob",
      text: "received while hidden",
      reply_to: null,
    });
  });
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
  await manage(page);
  await page.getByTestId(`restore-contact-${bob}`).click();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("hidden-contacts-dialog")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("settings-dialog")).toHaveCount(0);
  await page.getByTestId(`conversation-row-${bob}`).click();
  await expect(
    page.getByText("received while hidden", { exact: true }),
  ).toBeVisible();
});

test("offline hidden contacts can be restored from their saved identity", async ({
  page,
}) => {
  await login(page);
  await hideBob(page);
  await page.evaluate(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    ipc.invoke = async (cmd, args) =>
      cmd === "list_accounts" || cmd === "list_peers"
        ? []
        : original(cmd, args);
  });
  await expect(
    page.getByTestId("conversation-row-acc_carol_cccc3333"),
  ).toHaveCount(0, { timeout: 8000 });
  await manage(page);
  await expect(page.getByTestId(`restore-contact-${bob}`)).toBeVisible();
  await page.getByTestId(`restore-contact-${bob}`).click();
  await expect(page.getByTestId(`restore-contact-${bob}`)).toHaveCount(0);
  expect(
    await page.evaluate(() =>
      JSON.parse(localStorage.getItem("mock-hidden-u_self") ?? "[]"),
    ),
  ).toEqual([]);
});

test("contact visibility is isolated between local signed-in users", async ({
  page,
}) => {
  await login(page);
  await hideBob(page);
  await login(page, "other_user");
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeVisible();
  await login(page);
  await expect(
    page.getByTestId("conversation-row-acc_carol_cccc3333"),
  ).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
});

test("failed initial load offers retry without revealing hidden contacts", async ({
  page,
}) => {
  await page.goto("/");
  await page.evaluate(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    ipc.invoke = async (cmd, args) => {
      if (
        cmd === "get_hidden_contacts" &&
        !(window as unknown as Record<string, unknown>).__allowHiddenLoad
      ) {
        throw new Error("read failed");
      }
      return original(cmd, args);
    };
  });
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("a strong password");
  await page.getByTestId("login-submit").click();
  await expect(
    page.getByTestId("conversation-row-chan_team_dddd4444"),
  ).toBeVisible();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toHaveCount(0);
  await page.evaluate(() => {
    (window as unknown as Record<string, unknown>).__allowHiddenLoad = true;
  });
  await page
    .getByTestId("sidebar")
    .getByRole("button", { name: "Retry", exact: true })
    .click();
  await expect(page.getByTestId(`conversation-row-${bob}`)).toBeVisible();
});

test("narrow settings management remains usable with keyboard and no overflow", async ({
  page,
}, testInfo) => {
  await login(page);
  await hideBob(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await manage(page);
  const dialog = page.getByTestId("hidden-contacts-dialog");
  const bounds = await dialog.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
  await page.screenshot({
    path: testInfo.outputPath("hidden-contacts-management.png"),
    animations: "disabled",
  });
  await page.getByTestId(`restore-contact-${bob}`).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId(`restore-contact-${bob}`)).toHaveCount(0);
});
