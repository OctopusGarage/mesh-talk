import { test, expect } from "./tauri-mock";
import { enterChat, openBobDm, BOB, CHANNEL } from "./helpers/session";

test.use({ viewport: { width: 1100, height: 800 } });
test("sidebar, open header and group members share live names, remarks and offline profile identity", async ({
  page,
}) => {
  test.setTimeout(60_000);
  await enterChat(page);
  await openBobDm(page);
  const header = page.getByTestId("conversation-header");
  await expect(header.locator("img")).toBeVisible();
  await page.evaluate(({ account, device }) => {
    const win = window as unknown as {
      __identityName: string;
      __identityOffline: boolean;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    win.__identityName = "Renamed Bob";
    win.__identityOffline = false;
    const original = win.__TAURI_INTERNALS__.invoke;
    win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      const result = await original(cmd, args);
      if (cmd === "list_accounts")
        return (result as { account_id: string; names: string[] }[]).map(
          (entry) =>
            entry.account_id === account
              ? { ...entry, names: [win.__identityName, "Old Bob"] }
              : entry,
        );
      if (cmd === "list_peers" && win.__identityOffline)
        return (result as { user_id: string }[]).filter(
          (entry) => entry.user_id !== device,
        );
      if (cmd === "channel_members") {
        const snapshot = result as {
          members: { user_id: string; name: string; account_id?: string }[];
        };
        return {
          ...snapshot,
          members: snapshot.members.map((entry) =>
            entry.user_id === device
              ? { ...entry, account_id: account, name: "Old Bob" }
              : entry,
          ),
        };
      }
      if (cmd === "get_presence" && win.__identityOffline)
        return {
          ...(result as object),
          [account]: { online: false, last_seen_secs: 90 },
        };
      return result;
    };
  }, BOB);
  await expect(
    page.getByTestId(`conversation-row-${BOB.account}`),
  ).toContainText("Renamed Bob");
  await expect(header).toContainText("Renamed Bob");
  await expect(header).not.toContainText("Old Bob");
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  const member = page.getByTestId(`member-profile-${BOB.device}`);
  await expect(member).toContainText("Renamed Bob");
  await page.evaluate(() => {
    (window as unknown as { __identityName: string }).__identityName =
      "Newest Bob";
  });
  await expect(member).toContainText("Newest Bob");
  await page.evaluate(() => {
    (window as unknown as { __identityOffline: boolean }).__identityOffline =
      true;
  });
  await expect(member).toContainText("Offline");
  await expect(member.locator("img")).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(header).toContainText("Newest Bob");
  await expect(header.locator("img")).toBeVisible();
  await page.getByTestId(`conversation-actions-${BOB.account}`).click();
  await page.getByTestId(`conversation-rename-${BOB.account}`).click();
  const rename = page.getByRole("dialog");
  await rename.getByRole("textbox").fill("My colleague");
  await rename.getByRole("button", { name: "Save", exact: true }).click();
  await expect(header).toContainText("My colleague");
  await expect(
    page.getByTestId(`conversation-row-${BOB.account}`),
  ).toContainText("My colleague");
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.getByTestId("members-trigger").click();
  await expect(member).toContainText("My colleague");
  await member.click();
  const profile = page.getByTestId("member-profile-dialog");
  await expect(profile.getByTestId("member-profile-original-name")).toHaveText(
    "Newest Bob",
  );
  await expect(profile.getByTestId("member-profile-remark")).toHaveText(
    "My colleague",
  );
  await expect(profile).toContainText(BOB.account);
  await expect(profile).toContainText(BOB.device);
  await expect(profile.locator("img")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { __identityName: string }).__identityName =
      "Final Bob";
  });
  await expect(profile.getByTestId("member-profile-original-name")).toHaveText(
    "Final Bob",
  );
  await expect(profile.getByTestId("member-profile-remark")).toHaveText(
    "My colleague",
  );
});
