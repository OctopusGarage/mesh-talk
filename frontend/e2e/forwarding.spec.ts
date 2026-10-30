import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm } from "./helpers/session";

test("forwarding confirms recipients, keeps drafts and retries the original operation", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.getByTestId("composer-input").fill("Keep my unfinished draft");
  await page.evaluate(() => {
    const w = window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
      __forwardRequests: unknown[];
    };
    w.__forwardRequests = [];
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "owner_list_forward_batches") return [];
      if (cmd === "owner_inspect_forward_messages")
        return {
          source_label: "Bob",
          messages: [
            {
              id: "preview",
              sender_name: "Bob",
              who: "Bob",
              text: "hey, welcome to the mesh",
              wall_clock: 1,
              expires_at_ms: null,
              sticker: null,
              file: null,
            },
          ],
        };
      if (cmd === "owner_forward_messages") {
        const request = args.request as {
          operation_id: string;
          source: { message_ids: string[] };
          destinations: Array<{ kind: string; target: string }>;
        };
        w.__forwardRequests.push(request);
        return {
          operation_id: request.operation_id,
          items: request.destinations.map((target) => ({
            target,
            source_id: request.source.message_ids[0],
            message_id: w.__forwardRequests.length === 1 ? null : "accepted",
            status: w.__forwardRequests.length === 1 ? "failed" : "accepted",
            error:
              w.__forwardRequests.length === 1
                ? "Test route unavailable"
                : null,
          })),
        };
      }
      return original(cmd, args);
    };
  });
  await page
    .getByText("hey, welcome to the mesh", { exact: true })
    .click({ button: "right" });
  await page.getByTestId("msg-forward").click();
  const dialog = page.getByTestId("forward-dialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("searchbox").fill("carol");
  await dialog.getByRole("checkbox").first().check();
  await page.screenshot({
    path: test.info().outputPath("forward-picker-desktop.png"),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(async () => {
      const box = await dialog.boundingBox();
      return !!box && box.x >= 0 && box.x + box.width <= 390;
    })
    .toBe(true);
  await page.screenshot({
    path: test.info().outputPath("forward-picker-narrow.png"),
  });
  await page.setViewportSize({ width: 1280, height: 720 });
  await dialog.getByRole("button", { name: "Confirm and send" }).click();
  await expect(dialog.getByText("Test route unavailable")).toBeVisible();
  await dialog.getByRole("button", { name: "Retry unfinished items" }).click();
  await expect(
    dialog.getByText("Accepted for delivery", { exact: true }),
  ).toBeVisible();
  const requests = await page.evaluate(
    () =>
      (window as unknown as { __forwardRequests: unknown[] }).__forwardRequests,
  );
  expect(requests).toHaveLength(2);
  expect(requests[0]).toEqual(requests[1]);
  expect((requests[0] as { operation_id: string }).operation_id).toMatch(
    /^\d{13}-[0-9a-f-]{36}$/,
  );
  await dialog
    .getByRole("button", { name: "Close", exact: true })
    .first()
    .click();
  await expect(page.getByTestId("composer-input")).toHaveValue(
    "Keep my unfinished draft",
  );
  await page
    .getByText("hey, welcome to the mesh", { exact: true })
    .click({ button: "right" });
  await page.getByTestId("msg-select-messages").click();
  await page
    .locator(
      'input[data-testid^="favorite-select-"]:not(:checked):not(:disabled)',
    )
    .first()
    .check();
  await page
    .getByTestId("favorite-selection-toolbar")
    .getByRole("button", { name: "Forward…" })
    .click();
  await dialog.getByRole("radio", { name: "Merge as chat record" }).check();
  await dialog.getByRole("searchbox").fill("carol");
  await dialog.getByRole("checkbox").first().check();
  await dialog.getByRole("button", { name: "Confirm and send" }).click();
  await expect(
    dialog.getByText("Accepted for delivery", { exact: true }),
  ).toBeVisible();
  const merged = await page.evaluate(
    () =>
      (
        window as unknown as {
          __forwardRequests: Array<{
            mode: string;
            source: { message_ids: string[] };
          }>;
        }
      ).__forwardRequests[2],
  );
  expect(merged.mode).toBe("merged");
  expect(merged.source.message_ids).toHaveLength(2);
  await dialog
    .getByRole("button", { name: "Close", exact: true })
    .first()
    .click();
  await expect(page.getByTestId("favorite-selection-toolbar")).toHaveCount(0);
});

// Real React composition/decoding/export dispatch; IPC transport is substituted here.
test("received merged records render ordered content and media, export attachments and reject invalid archives", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    type Probe = {
      bytes: ArrayBuffer;
      invalid: boolean;
      exports: Array<Record<string, unknown>>;
    };
    const w = window as unknown as {
      __recordProbe: Probe;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    const canvas = document.createElement("canvas");
    canvas.width = 32;
    canvas.height = 16;
    canvas.getContext("2d")!.fillRect(0, 0, 32, 16);
    const png = canvas.toDataURL("image/png").split(",")[1];
    const record = {
      format: "MTCHAT1",
      title: "Forwarded design discussion",
      created_at_ms: 1700000000000,
      messages: [
        {
          sender_name: "Alice",
          wall_clock: 1700000000000,
          text: "First: <script>window.untrustedRecordRan=true</script>",
          file: null,
        },
        {
          sender_name: "Bob",
          wall_clock: 1700000001000,
          text: "Second: reference image",
          file: {
            name: "reference.png",
            mime: "image/png",
            size: atob(png).length,
            data_base64: png,
          },
        },
        {
          sender_name: "Carol",
          wall_clock: 1700000002000,
          text: "Third: attached notes",
          file: {
            name: "notes.txt",
            mime: "text/plain",
            size: 5,
            data_base64: btoa("notes"),
          },
        },
      ],
    };
    w.__recordProbe = {
      bytes: new TextEncoder().encode(JSON.stringify(record)).buffer,
      invalid: false,
      exports: [],
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (cmd === "file_statuses")
        return (args.fileConvs as string[]).map((file_conv) => ({
          file_conv,
          done: 1,
          total: 1,
          ready: true,
        }));
      if (
        (cmd === "read_media" || cmd === "read_file") &&
        args.fileConv === "received-merged-record"
      )
        return w.__recordProbe.invalid
          ? new TextEncoder().encode('{"format":"untrusted"}').buffer
          : w.__recordProbe.bytes;
      if (cmd === "plugin:dialog|save") return "/tmp/forwarded-notes.txt";
      if (cmd === "owner_export_forwarded_attachment") {
        w.__recordProbe.exports.push(args);
        return "/tmp/forwarded-notes.txt";
      }
      const result = await original(cmd, args);
      if (
        cmd === "owner_account_history" &&
        args.account === "acc_bob_bbbb2222"
      )
        return [
          ...(result as unknown[]),
          {
            id: "received-merged-record-message",
            from_me: false,
            who: "device_bob_2222",
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              name: "discussion.mtchat",
              size: w.__recordProbe.bytes.byteLength,
              mime: "application/vnd.mesh-talk.chat+json",
              file_conv: "received-merged-record",
              media: false,
            },
          },
        ];
      return result;
    };
  });
  await openBobDm(page);
  await page.getByTestId("forwarded-record-open").click();
  const viewer = page.getByTestId("forwarded-record-viewer");
  await expect(
    viewer.getByRole("heading", { name: "Forwarded design discussion" }),
  ).toBeVisible();
  const rows = viewer
    .getByTestId("forwarded-record-messages")
    .locator(":scope > li");
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toContainText("Alice");
  await expect(rows.nth(1)).toContainText("Bob");
  await expect(rows.nth(2)).toContainText("Carol");
  await expect(rows.nth(0)).toContainText(
    "<script>window.untrustedRecordRan=true</script>",
  );
  await expect(
    viewer.getByRole("img", { name: "reference.png" }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { untrustedRecordRan?: boolean })
          .untrustedRecordRan,
    ),
  ).toBeUndefined();
  await viewer
    .getByRole("img", { name: "reference.png" })
    .evaluate(async (image) => {
      await (image as HTMLImageElement).decode();
    });
  await page.screenshot({
    path: test.info().outputPath("merged-viewer-desktop.png"),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(async () => {
      const box = await viewer.boundingBox();
      return !!box && box.x >= 0 && box.x + box.width <= 390;
    })
    .toBe(true);
  await page.screenshot({
    path: test.info().outputPath("merged-viewer-narrow.png"),
  });
  await rows.nth(2).getByRole("button").click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __recordProbe: { exports: unknown[] } })
            .__recordProbe.exports.length,
      ),
    )
    .toBe(1);
  const exported = await page.evaluate(
    () =>
      (
        window as unknown as {
          __recordProbe: { exports: Record<string, unknown>[] };
        }
      ).__recordProbe.exports[0],
  );
  expect(exported).toMatchObject({
    fileConv: "received-merged-record",
    index: 2,
    destination: "/tmp/forwarded-notes.txt",
  });
  await viewer.getByRole("button", { name: "Close", exact: true }).click();
  await page.evaluate(() => {
    (
      window as unknown as { __recordProbe: { invalid: boolean } }
    ).__recordProbe.invalid = true;
  });
  await page.getByTestId("forwarded-record-open").click();
  await expect(viewer.getByRole("alert")).toBeVisible();
  await expect(viewer.getByTestId("forwarded-record-messages")).toHaveCount(0);
});
