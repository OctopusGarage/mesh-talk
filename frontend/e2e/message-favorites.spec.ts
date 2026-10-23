import { test, expect, type FavoriteAttachmentMock } from "./tauri-mock";
import type { Page } from "@playwright/test";
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

async function openRetainedAttachment(
  page: Page,
  mode = "favorites-attachment",
) {
  await enterChat(page, "tester", `/?data=${mode}`);
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  return page.getByTestId("message-favorites");
}

async function retainedAttachmentState(page: Page) {
  return page.evaluate(() => {
    const { bytes, calls, saved } = (
      window as unknown as { __favoriteAttachment: FavoriteAttachmentMock }
    ).__favoriteAttachment;
    return { bytes, calls, saved };
  });
}

test("retained attachments preview real bytes and save independently of source history", async ({
  page,
}) => {
  const dialog = await openRetainedAttachment(page);
  await expect(dialog).toContainText("Keep for the field report");
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  const image = dialog.getByRole("img", { name: "field-photo.png" });
  await expect(image).toBeVisible();
  const preview = await image.evaluate(async (element) => {
    const image = element as HTMLImageElement;
    await image.decode();
    return {
      width: image.naturalWidth,
      height: image.naturalHeight,
      bytes: Array.from(
        new Uint8Array(await (await fetch(image.src)).arrayBuffer()),
      ),
    };
  });
  const expectedBytes = (await retainedAttachmentState(page)).bytes;
  expect(preview).toEqual({ width: 2, height: 2, bytes: expectedBytes });
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Saved");
  const state = await retainedAttachmentState(page);
  expect(state.saved).toEqual([
    { path: "/home/tester/Downloads/field-photo.png", bytes: expectedBytes },
  ]);
  expect(state.calls).toContainEqual({
    command: "owner_read_favorite_file",
    args: { owner: "u_self", fileConv: "retained-photo" },
  });
  expect(state.calls).toContainEqual({
    command: "owner_save_favorite_file",
    args: {
      owner: "u_self",
      fileConv: "retained-photo",
      dest: "/home/tester/Downloads/field-photo.png",
      toDirectory: false,
    },
  });
  expect(
    state.calls.some(({ command }) =>
      ["read_media", "read_file", "save_file", "save_file_to_dir"].includes(
        command,
      ),
    ),
  ).toBe(false);

  // Removing the last favorite reference retires its displayed plaintext preview.
  const previewUrl = await image.getAttribute("src");
  await dialog
    .getByRole("button", { name: "Remove favorite", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Remove favorite", exact: true })
    .last()
    .click();
  await expect(page.getByTestId("favorite-item")).toHaveCount(0);
  await expect(image).toBeHidden();
  await expect
    .poll(() =>
      page.evaluate(async (url) => {
        try {
          await fetch(url!);
          return false;
        } catch {
          return true;
        }
      }, previewUrl),
    )
    .toBe(true);
  const available = await page.evaluate(() =>
    (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            command: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__.invoke("owner_favorite_file_available", {
      owner: "u_self",
      fileConv: "retained-photo",
    }),
  );
  expect(available).toBe(false);
});

test("retained attachment save rechecks authorization after the native dialog", async ({
  page,
}) => {
  const dialog = await openRetainedAttachment(
    page,
    "favorites-attachment-delayed-dialog",
  );
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (
            window as unknown as {
              __favoriteAttachment: FavoriteAttachmentMock;
            }
          ).__favoriteAttachment.releaseSaveDialog,
      ),
    )
    .toBe("function");
  const callsBeforeReturn = (await retainedAttachmentState(page)).calls.length;
  await page.evaluate(() => {
    const fixture = (
      window as unknown as { __favoriteAttachment: FavoriteAttachmentMock }
    ).__favoriteAttachment;
    fixture.authorized = false;
    fixture.releaseSaveDialog!();
  });
  await expect(
    dialog.getByRole("button", { name: "Save", exact: true }),
  ).toBeEnabled();
  await expect(dialog.getByRole("status")).toHaveCount(0);
  const state = await retainedAttachmentState(page);
  expect(state.calls.slice(callsBeforeReturn)).toEqual([
    {
      command: "owner_favorite_file_available",
      args: { owner: "u_self", fileConv: "retained-photo" },
    },
  ]);
  expect(
    state.calls.some(({ command }) =>
      ["owner_save_favorite_file", "save_file", "save_file_to_dir"].includes(
        command,
      ),
    ),
  ).toBe(false);
  expect(state.saved).toEqual([]);
  await page.keyboard.press("Escape");
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  await expect(
    dialog.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await expect(
    dialog.getByRole("button", { name: "Preview", exact: true }),
  ).toBeDisabled();
});

test("closing Favorites retires an in-flight retained attachment preview", async ({
  page,
}) => {
  const dialog = await openRetainedAttachment(
    page,
    "favorites-attachment-delayed-read",
  );
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          typeof (
            window as unknown as {
              __favoriteAttachment: FavoriteAttachmentMock;
            }
          ).__favoriteAttachment.releaseRead,
      ),
    )
    .toBe("function");
  await page.keyboard.press("Escape");
  await page.getByTestId("open-message-favorites").click();
  await page.getByTestId("favorite-item").click();
  const urls = await page.evaluate(async () => {
    const created: string[] = [];
    const create = URL.createObjectURL;
    URL.createObjectURL = (blob) => {
      const url = create(blob);
      created.push(url);
      return url;
    };
    try {
      (window as unknown as { __favoriteAttachment: FavoriteAttachmentMock })
        .__favoriteAttachment.releaseRead!();
      // IPC promise continuations drain before the next browser frame.
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => resolve()),
      );
      return created;
    } finally {
      URL.createObjectURL = create;
    }
  });
  expect(urls).toEqual([]);
  await expect(
    dialog.getByRole("img", { name: "field-photo.png" }),
  ).toHaveCount(0);
  await expect(
    dialog.getByRole("button", { name: "Preview", exact: true }),
  ).toBeEnabled();
});

test("retained preview denies lost authorization without falling back to chat media", async ({
  page,
}) => {
  const dialog = await openRetainedAttachment(page);
  await expect(
    dialog.getByRole("button", { name: "Preview", exact: true }),
  ).toBeEnabled();
  await page.evaluate(() => {
    (
      window as unknown as { __favoriteAttachment: FavoriteAttachmentMock }
    ).__favoriteAttachment.authorized = false;
  });
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "Favorite file unavailable",
  );
  await expect(
    dialog.getByRole("img", { name: "field-photo.png" }),
  ).toHaveCount(0);
  const { calls } = await retainedAttachmentState(page);
  expect(
    calls.filter(({ command }) => command === "owner_read_favorite_file"),
  ).toHaveLength(1);
  expect(
    calls.some(({ command }) => ["read_media", "read_file"].includes(command)),
  ).toBe(false);
});
