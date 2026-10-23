import { test, expect } from "./tauri-mock";
import { BOB, CHANNEL, enterChat, openBobDm } from "./helpers/session";

test("collects related messages with searchable overall and individual notes", async ({
  page,
}, testInfo) => {
  await enterChat(page);
  await openBobDm(page);
  const first = page.getByRole("log").getByText("hey, welcome to the mesh");
  await first.click({ button: "right" });
  await page.getByTestId("msg-select-messages").click();
  const boxes = page.getByRole("log").locator('input[type="checkbox"]');
  await boxes.nth(1).check();
  await page
    .getByTestId("favorite-selection-toolbar")
    .getByRole("button", { name: "Collect", exact: true })
    .click();
  await page.getByTestId("favorite-title").fill("Deployment runbook");
  await page
    .getByTestId("favorite-description")
    .fill("Customer A troubleshooting");
  await page
    .getByTestId("favorite-editor")
    .getByText("Message note", { exact: true })
    .first()
    .click();
  await page.getByTestId("favorite-note-0").fill("Check firewall first");
  await page.getByTestId("favorite-save").click();
  await expect(page.getByTestId("favorite-editor")).toBeHidden();
  await expect(page.getByTestId("favorite-selection-toolbar")).toBeHidden();
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorites-search").fill("firewall");
  await expect(page.getByTestId("favorite-item")).toHaveCount(1);
  await expect(page.getByTestId("favorite-item")).toContainText(
    "Check firewall first",
  );
  await page.getByTestId("favorite-item").click();
  await expect(page.getByTestId("message-favorites")).toContainText(
    "Customer A troubleshooting",
  );
  await page.screenshot({ path: testInfo.outputPath("favorites-desktop.png") });
  await page.getByRole("button", { name: "Edit notes", exact: true }).click();
  await page
    .getByTestId("favorite-description")
    .fill("Updated deployment details");
  await page.getByTestId("favorite-save").click();
  await page.getByTestId("favorites-search").fill("Updated deployment");
  await expect(page.getByTestId("favorite-item")).toHaveCount(1);
  await page.getByTestId("favorite-item").click();
  await page
    .getByRole("button", { name: "Remove favorite", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Remove favorite", exact: true })
    .last()
    .click();
  await expect(page.getByTestId("favorite-item")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(first).toBeVisible();
});

test("single favorites preserve original navigation and selection resets between conversations", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await page.getByTestId("favorite-description").fill("Reference");
  await page.getByTestId("favorite-save").click();
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  await page
    .getByRole("button", { name: "View original", exact: true })
    .click();
  await expect(page.getByTestId("message-favorites")).toBeHidden();
  await expect(
    page.getByRole("log").getByText("hey, welcome to the mesh"),
  ).toBeVisible();
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await expect(page.getByTestId("favorite-description")).toHaveValue(
    "Reference",
  );
  await page
    .getByTestId("favorite-editor")
    .getByRole("button", { name: "Cancel", exact: true })
    .click();
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-select-messages").click();
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await expect(page.getByTestId("favorite-selection-toolbar")).toBeHidden();
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("favorite-selection-toolbar")).toBeHidden();
});

test("Favorites works with keyboard entry and a narrow window", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 760, height: 640 });
  await enterChat(page);
  const trigger = page.getByTestId("open-message-favorites");
  await trigger.focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByText("No favorites yet", { exact: true }),
  ).toBeVisible();
  const bounds = await page.getByTestId("message-favorites").boundingBox();
  expect(bounds!.width).toBeLessThanOrEqual(760);
  await page.screenshot({ path: testInfo.outputPath("favorites-narrow.png") });
  await page.keyboard.press("Escape");
  await expect(trigger).toBeFocused();
});

test("group favorites retain saved content when their original is removed", async ({
  page,
}) => {
  await enterChat(page);
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  const original = page
    .getByRole("log")
    .getByText("channel kickoff", { exact: true });
  await original.click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await page.getByTestId("favorite-title").fill("Team decision");
  await page.getByTestId("favorite-save").click();
  await expect(page.getByTestId("favorite-editor")).toBeHidden();
  await original.click({ button: "right" });
  await page.getByTestId("msg-delete").click();
  await page.getByTestId("delete-message-confirm").click();
  await expect(original).toBeHidden();
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  await expect(page.getByTestId("message-favorites")).toContainText(
    "channel kickoff",
  );
  await page
    .getByRole("button", { name: "View original", exact: true })
    .click();
  await expect(
    page.getByTestId("message-favorites").getByRole("alert"),
  ).toBeVisible();
  await expect(page.getByTestId("message-favorites")).toBeVisible();
});

test("failed durable save preserves the user's notes for retry", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=favorites-write-fail");
  await openBobDm(page);
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await page
    .getByTestId("favorite-description")
    .fill("Do not lose this explanation");
  await page.getByTestId("favorite-save").click();
  await expect(
    page.getByTestId("favorite-editor").getByRole("alert"),
  ).toContainText("disk full");
  await expect(page.getByTestId("favorite-description")).toHaveValue(
    "Do not lose this explanation",
  );
  await expect(page.getByTestId("favorite-save")).toBeEnabled();
});

test("closing Favorites retires pending original-message navigation", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=favorites-delayed-context");
  await openBobDm(page);
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await page.getByTestId("favorite-save").click();
  await expect(page.getByTestId("favorite-editor")).toBeHidden();
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  await page
    .getByRole("button", { name: "View original", exact: true })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (
            window as unknown as { __releaseFavoriteContext?: () => void }
          ).__releaseFavoriteContext,
      ),
    )
    .toBe("function");
  await page.keyboard.press("Escape");
  await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
  await page.evaluate(async () => {
    (
      window as unknown as { __releaseFavoriteContext: () => void }
    ).__releaseFavoriteContext();
    await new Promise((resolve) => setTimeout(resolve, 100));
  });
  await expect(
    page.getByRole("log").getByText("channel kickoff", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("log").getByText("hey, welcome to the mesh"),
  ).toBeHidden();
});

test("editing supersedes pending original navigation without leaving actions disabled", async ({
  page,
}) => {
  await enterChat(page, "tester", "/?data=favorites-delayed-context");
  await openBobDm(page);
  await page
    .getByRole("log")
    .getByText("hey, welcome to the mesh")
    .click({ button: "right" });
  await page.getByTestId("msg-collect").click();
  await page.getByTestId("favorite-save").click();
  await expect(page.getByTestId("favorite-editor")).toBeHidden();
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  await page
    .getByRole("button", { name: "View original", exact: true })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (
            window as unknown as { __releaseFavoriteContext?: () => void }
          ).__releaseFavoriteContext,
      ),
    )
    .toBe("function");
  await page.getByRole("button", { name: "Edit notes", exact: true }).click();
  await page
    .getByTestId("favorite-editor")
    .getByRole("button", { name: "Cancel", exact: true })
    .click();
  await page.getByTestId("favorite-item").click();
  await expect(
    page.getByRole("button", { name: "View original", exact: true }),
  ).toBeEnabled();
  await page.evaluate(async () => {
    (
      window as unknown as { __releaseFavoriteContext: () => void }
    ).__releaseFavoriteContext();
    await new Promise((resolve) => setTimeout(resolve, 100));
  });
  await expect(page.getByTestId("message-favorites")).toBeVisible();
});
