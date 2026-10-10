import { test, expect } from "./tauri-mock";

test("pasted file waits for Enter and transfers as an attachment", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("login-tab-register").click();
  await page.getByTestId("login-username").fill("paster");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await page.getByTestId("login-tab-signin").click();
  await page.getByTestId("login-username").fill("paster");
  await page.getByTestId("login-password").fill("password123");
  await page.getByTestId("login-submit").click();
  await page.getByTestId("conversation-row-acc_bob_bbbb2222").click();
  await expect(page.getByTestId("composer-input")).toBeVisible();
  await expect(
    page.getByRole("log").getByText("hey, welcome to the mesh"),
  ).toBeVisible();

  const paste = await page.evaluate(() => {
    const tauri = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      }
    ).__TAURI_INTERNALS__;
    const seen: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    (window as unknown as { __pasteCommands: typeof seen }).__pasteCommands =
      seen;
    const original = tauri.invoke.bind(tauri);
    tauri.invoke = async (cmd, args) => {
      seen.push({ cmd, args });
      if (cmd === "plugin:dialog|save")
        return "/home/tester/Downloads/report.txt";
      return original(cmd, args);
    };
    const input = document.querySelector<HTMLTextAreaElement>(
      "[data-testid=composer-input]",
    )!;
    input.focus();
    const file = new File(["hello"], "report.txt", { type: "text/plain" });
    const transfer = new DataTransfer();
    transfer.items.add(file);
    transfer.setData("text/plain", "report.txt");
    const event = new ClipboardEvent("paste", {
      bubbles: true,
      cancelable: true,
      clipboardData: transfer,
    });
    input.dispatchEvent(event);
    return {
      files: event.clipboardData?.files.length,
      prevented: event.defaultPrevented,
    };
  });
  expect(paste).toEqual({ files: 1, prevented: true });
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "report.txt",
  );
  await expect(page.getByTestId("composer-input")).toHaveValue("");
  expect(
    await page.evaluate(() =>
      (
        window as unknown as { __pasteCommands: Array<{ cmd: string }> }
      ).__pasteCommands.some((c) => c.cmd === "owner_enqueue_file"),
    ),
  ).toBe(false);
  await page.getByTestId("composer-input").press("Enter");
  await expect(page.getByTestId("composer-pending-files")).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as {
            __pasteCommands: Array<{
              cmd: string;
              args: Record<string, unknown>;
            }>;
          }
        ).__pasteCommands.some(
          (c) => c.cmd === "owner_enqueue_file" && c.args.media === false,
        ),
      ),
    )
    .toBe(true);
  const commands = await page.evaluate(
    () =>
      (
        window as unknown as {
          __pasteCommands: Array<{
            cmd: string;
            args: Record<string, unknown>;
          }>;
        }
      ).__pasteCommands,
  );
  expect(
    commands.some(
      (c) => c.cmd === "send_to_account" && c.args.text === "report.txt",
    ),
  ).toBe(false);

  const fileBubble = page
    .getByTestId("message-bubble")
    .filter({ hasText: "report.txt" });
  await expect(fileBubble).toBeVisible();
  await fileBubble.getByRole("button", { name: "Save" }).click();
  await expect(fileBubble).toContainText("/home/tester/Downloads/report.txt");
  await fileBubble.getByRole("button", { name: "Reveal" }).click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as { __pasteCommands: Array<{ cmd: string }> }
        ).__pasteCommands.some(
          (c) => c.cmd === "plugin:opener|reveal_item_in_dir",
        ),
      ),
    )
    .toBe(true);

  await page.evaluate(() => {
    const input = document.querySelector<HTMLTextAreaElement>(
      "[data-testid=composer-input]",
    )!;
    const transfer = new DataTransfer();
    transfer.items.add(new File(["png"], "photo.png", { type: "image/png" }));
    input.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "photo.png",
  );
  await page.getByTestId("composer-send").click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as unknown as {
            __pasteCommands: Array<{
              cmd: string;
              args: Record<string, unknown>;
            }>;
          }
        ).__pasteCommands.some(
          (c) => c.cmd === "owner_enqueue_file" && c.args.media === true,
        ),
      ),
    )
    .toBe(true);

  await expect(page.getByTestId("composer-pending-files")).toHaveCount(0);
  await page.evaluate(() => {
    const input = document.querySelector<HTMLTextAreaElement>(
      "[data-testid=composer-input]",
    )!;
    const transfer = new DataTransfer();
    transfer.items.add(
      new File(["skip"], "unsent.txt", { type: "text/plain" }),
    );
    input.dispatchEvent(
      new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      }),
    );
  });
  await expect(page.getByTestId("composer-pending-files")).toContainText(
    "unsent.txt",
  );
  await page.getByRole("button", { name: "Dismiss unsent.txt" }).click();
  await expect(page.getByTestId("composer-pending-files")).toHaveCount(0);
});
