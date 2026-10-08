import { test, expect } from "./tauri-mock";
const bob = "b".repeat(32);
test.beforeEach(async ({ page }) => {
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
    ipc.invoke = async (command, args) => {
      if (command === "list_accounts")
        return [
          { account_id: "b".repeat(32), names: ["Bob"], device_count: 2 },
        ];
      if (
        !["get_privacy", "set_invisible", "set_privacy_allowed"].includes(
          command,
        )
      )
        return original(command, args);
      const key = `privacy-e2e-${String(args.owner)}`;
      const saved = localStorage.getItem(key);
      const state = saved
        ? JSON.parse(saved)
        : {
            owner: args.owner,
            version: 1,
            invisible: false,
            allowed_accounts: [],
          };
      if (command === "set_invisible") state.invisible = args.invisible;
      if (command === "set_privacy_allowed")
        state.allowed_accounts = args.allowed
          ? [{ id: args.account, name: "Bob", source: "Manual" }]
          : [];
      localStorage.setItem(key, JSON.stringify(state));
      return state;
    };
  });
});
async function settings(page: import("@playwright/test").Page) {
  await page.goto("/");
  await page.getByTestId("login-username").fill("tester");
  await page.getByTestId("login-password").fill("a strong password");
  await page.getByTestId("login-submit").click();
  await page.getByTestId("sidebar-nav-settings").click();
}
test("invisible mode requires confirmation and survives reload", async ({
  page,
}) => {
  await settings(page);
  const toggle = page.getByTestId("invisible-switch");
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(page.getByTestId("invisible-cancel")).toBeFocused();
  await expect(
    page.getByText(
      "Offline delivery requires a manually allowed trusted relay account. Missing verified sender identity is rejected; old relay versions may not support delivery while invisible.",
    ),
  ).toBeVisible();
  await page.getByTestId("invisible-cancel").click();
  await expect(toggle).toBeFocused();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await page.getByTestId("invisible-confirm").click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await page.reload();
  await settings(page);
  await expect(page.getByTestId("invisible-switch")).toHaveAttribute(
    "aria-checked",
    "true",
  );
});
test("permission management supports search, grant and confirmed revoke", async ({
  page,
}) => {
  await settings(page);
  await page.getByTestId("manage-privacy").click();
  await page.getByTestId("privacy-search").fill("Bob");
  await page.getByTestId(`privacy-allow-${bob}`).click();
  await expect(page.getByTestId(`privacy-revoke-${bob}`)).toBeVisible();
  await page.getByTestId(`privacy-revoke-${bob}`).click();
  await expect(page.getByTestId("privacy-revoke-cancel")).toBeFocused();
  await page.getByTestId("privacy-revoke-cancel").click();
  await expect(page.getByTestId(`privacy-revoke-${bob}`)).toBeFocused();
  await expect(page.getByTestId(`privacy-revoke-${bob}`)).toBeVisible();
  await page.getByTestId(`privacy-revoke-${bob}`).click();
  await page.getByTestId("privacy-revoke-confirm").click();
  await expect(page.getByTestId(`privacy-allow-${bob}`)).toBeVisible();
});

test("failed load disables privacy controls and offers retry", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    const flags = window as unknown as { privacyLoadFails: boolean };
    flags.privacyLoadFails = true;
    ipc.invoke = async (c, a) => {
      if (c === "get_privacy" && flags.privacyLoadFails)
        throw new Error("cannot read");
      return original(c, a);
    };
  });
  await settings(page);
  await expect(page.getByTestId("invisible-switch")).toBeDisabled();
  await expect(page.getByTestId("manage-privacy")).toBeDisabled();
  await page.evaluate(() => {
    (window as unknown as { privacyLoadFails: boolean }).privacyLoadFails =
      false;
  });
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByTestId("invisible-switch")).toBeEnabled();
});

test("failed mode save keeps the old state and supports retry", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const ipc = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (c: string, a: Record<string, unknown>) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const original = ipc.invoke;
    let fail = true;
    ipc.invoke = async (c, a) => {
      if (c === "set_invisible" && fail) {
        fail = false;
        throw new Error("disk full");
      }
      return original(c, a);
    };
  });
  await settings(page);
  await page.getByTestId("invisible-switch").click();
  await page.getByTestId("invisible-confirm").click();
  await expect(page.getByTestId("invisible-switch")).toHaveAttribute(
    "aria-checked",
    "false",
  );
  await expect(page.getByTestId("invisible-confirm")).toBeVisible();
  await expect(
    page
      .getByText(
        "Could not save the change. The previous permission remains in effect; retry.",
      )
      .first(),
  ).toBeVisible();
  await page.getByTestId("invisible-confirm").click();
  await expect(page.getByTestId("invisible-switch")).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

test("offline initiated permissions remain manageable in a narrow layout", async ({
  page,
}) => {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.addInitScript(() => {
    localStorage.setItem(
      "privacy-e2e-u_self",
      JSON.stringify({
        owner: "u_self",
        version: 1,
        invisible: true,
        allowed_accounts: [
          {
            id: "d".repeat(32),
            name: "Dormant offline contact",
            source: "Initiated",
          },
        ],
      }),
    );
  });
  await settings(page);
  await page.getByTestId("manage-privacy").click();
  await page.getByTestId("privacy-search").fill("Dormant");
  await expect(
    page.getByText("Allowed after you contacted them"),
  ).toBeVisible();
  const revoke = page.getByTestId(`privacy-revoke-${"d".repeat(32)}`);
  await expect(revoke).toBeVisible();
  const width = await page
    .getByTestId("privacy-dialog")
    .evaluate((e) => e.scrollWidth <= e.clientWidth);
  expect(width).toBe(true);
  await revoke.click();
  await page.getByTestId("privacy-revoke-confirm").click();
  await expect(revoke).toHaveCount(0);
});
