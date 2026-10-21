import { readFile } from "node:fs/promises";
import { waitForChatShell } from "./helpers/session";
import { test, expect } from "./tauri-mock";
test.use({ viewport: { width: 1100, height: 800 } });
async function login(page: import("@playwright/test").Page) {
  await page.goto("/");
  for (const tab of ["register", "signin"]) {
    await page.getByTestId(`login-tab-${tab}`).click();
    await page.getByTestId("login-username").fill("tester");
    await page.getByTestId("login-password").fill("password123");
    await page.getByTestId("login-submit").click();
  }
  await waitForChatShell(page);
}
test("publishes a signed profile without inventing a creation date and exports only public JSON", async ({
  page,
}, testInfo) => {
  test.setTimeout(60_000);
  await login(page);
  await page.getByTestId("open-profile").click();
  const panel = page.getByTestId("trust-card-panel");
  await expect(
    panel.getByText("No signed card available.", { exact: false }),
  ).toBeVisible();
  await panel.getByLabel("Signed display name").fill("Alice");
  await panel.getByLabel("Bio", { exact: true }).fill("A public introduction");
  await panel.getByRole("button", { name: "Publish signed card" }).click();
  await expect(panel.getByTestId("signed-card-details")).toContainText(
    "Account creation date: unknown",
  );
  await panel.getByText("Account ID and QR code", { exact: true }).click();
  await expect(
    panel.getByText("This QR code contains only the account ID."),
  ).toBeVisible();
  const download = page.waitForEvent("download");
  await panel.getByRole("button", { name: "Download signed card" }).click();
  const artifact = await download;
  expect(artifact.suggestedFilename()).toBe(
    "mesh-talk-trust-acc_self_aaaa1111.json",
  );
  const content = await readFile((await artifact.path())!, "utf8");
  expect(JSON.parse(content)).toMatchObject({
    schema: 1,
    kind: "card",
    payload: { display_name: "Alice", created_at_ms: null },
  });
  for (const field of ["verified", "first_seen", "device_changes"])
    expect(content).not.toContain(field);
  await panel.getByTestId("signed-card-details").scrollIntoViewIfNeeded();
  await page.screenshot({
    path: testInfo.outputPath("trust-card-desktop.png"),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await panel.getByTestId("signed-card-details").scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath("trust-card-mobile.png") });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.setViewportSize({ width: 1100, height: 800 });
  await panel
    .getByText("Import a signed card or proof", { exact: true })
    .click();
  await panel
    .getByLabel("Signed artifact JSON")
    .fill('{"schema":1,"kind":"card","payload":{"invalid_signature":true}}');
  await panel.getByRole("button", { name: "Import signed JSON" }).click();
  await expect(panel.getByRole("alert")).toContainText("Invalid signature");
});
test("imports a contact card and creates a proof without changing local verification", async ({
  page,
}) => {
  await login(page);
  await page.getByTestId("open-profile").click();
  const ownPanel = page.getByTestId("trust-card-panel");
  await ownPanel
    .getByText("Import a signed card or proof", { exact: true })
    .click();
  await ownPanel.getByLabel("Signed artifact JSON").fill(
    JSON.stringify({
      schema: 1,
      kind: "card",
      payload: {
        account_id: "acc_bob_bbbb2222",
        display_name: "Bob signed",
        bio: "Hello",
        created_at_ms: 1700000000000,
      },
    }),
  );
  await ownPanel.getByRole("button", { name: "Import signed JSON" }).click();
  await expect(ownPanel.getByLabel("Signed artifact JSON")).toHaveValue("");
  await page.keyboard.press("Escape");
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await page.getByTestId("verify-trigger").click();
  const panel = page.getByTestId("trust-card-panel");
  await expect(panel.getByTestId("signed-card-details")).toContainText(
    "Bob signed",
  );
  await panel.getByText("Create a shareable proof", { exact: true }).click();
  await panel.getByLabel("Introduction note").fill("Met in person");
  await panel
    .getByRole("button", { name: "Create introduction", exact: true })
    .click();
  await expect(panel.getByLabel("Signed proof JSON")).toHaveValue(
    /Met in person/,
  );
  await panel
    .getByRole("button", { name: "Witness creation", exact: true })
    .click();
  await expect(panel.getByLabel("Signed proof JSON")).toHaveValue(/witness/);
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("verify-trigger")).toHaveAttribute(
    "data-trust",
    "unverified",
  );
});
