import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB } from "./helpers/session";

type Diagnostic = {
  available: boolean;
  calls: Array<{ cmd: string; available: boolean; rejected: boolean }>;
  identity: string;
  historyCalls: number;
  png: ArrayBuffer;
  actions: Array<{ cmd: string; args: Record<string, unknown> }>;
  copiedImage: number[] | null;
  copyError: string | null;
};
type MediaWindow = Window & {
  __mediaDiagnostic: Diagnostic;
  __TAURI_INTERNALS__: {
    invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  };
};

test("received media recovers when bytes become available with stable fileConv", async ({
  page,
  browserName,
}) => {
  // Chromium exercises the Mac integration wiring, not Apple's recognition engine.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "userAgent", {
      get: () => "Macintosh; Mac OS X",
    });
  });
  if (browserName === "chromium")
    await page
      .context()
      .grantPermissions(["clipboard-read", "clipboard-write"]);
  await enterChat(page);
  await page.evaluate(() => {
    const w = window as unknown as MediaWindow;
    w.__mediaDiagnostic = {
      available: false,
      calls: [],
      identity: "fc_early_media_diagnostic",
      historyCalls: 0,
      png: new ArrayBuffer(0),
      actions: [],
      copiedImage: null,
      copyError: null,
    };
    // Preserve the real gesture-triggered OS write. WebKit does not expose
    // Chromium's automated clipboard permissions, so also observe its result
    // and decode the exact ClipboardItem submitted to the native writer.
    const write = navigator.clipboard.write.bind(navigator.clipboard);
    navigator.clipboard.write = (items) => {
      const written = write(items);
      void written
        .then(async () => {
          const png = await items[0].getType("image/png");
          const bitmap = await createImageBitmap(png);
          w.__mediaDiagnostic.copiedImage = [bitmap.width, bitmap.height];
          bitmap.close();
        })
        .catch((error) => {
          w.__mediaDiagnostic.copyError = String(error);
        });
      return written;
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (
      cmd: string,
      args: Record<string, unknown>,
    ) => {
      const d = w.__mediaDiagnostic;
      if (
        [
          "plugin:dialog|save",
          "save_file",
          "plugin:opener|reveal_item_in_dir",
        ].includes(cmd)
      ) {
        d.actions.push({ cmd, args });
        return cmd === "plugin:dialog|save" ? "/tmp/image-menu-test.png" : null;
      }
      if (cmd === "file_statuses") {
        return (args.fileConvs as string[]).map((file_conv) =>
          file_conv === d.identity
            ? {
                file_conv,
                done: d.available ? 4 : 2,
                total: 4,
                ready: d.available,
              }
            : { file_conv, done: 1, total: 1, ready: true },
        );
      }
      if (
        (cmd === "read_media" || cmd === "read_file") &&
        args.fileConv === d.identity
      ) {
        d.calls.push({ cmd, available: d.available, rejected: !d.available });
        if (!d.available) throw new Error("diagnostic bytes not available yet");
        return d.png;
      }
      const result = await original(cmd, args);
      if (
        cmd === "owner_account_history" &&
        args.account === "acc_bob_bbbb2222"
      ) {
        d.historyCalls++;
        return [
          ...(result as unknown[]),
          {
            id: "early-media-stable-message",
            from_me: false,
            who: "device_bob_2222",
            text: "",
            wall_clock: Date.now(),
            reply_to: null,
            file: {
              name: "late-bytes.png",
              size: 68,
              mime: "image/png",
              file_conv: d.identity,
              media: true,
            },
          },
        ];
      }
      return result;
    };
    // Produce valid PNG bytes with the browser's encoder, independent of IPC mock.
    const canvas = document.createElement("canvas");
    canvas.width = 1200;
    canvas.height = 900;
    canvas.getContext("2d")!.fillRect(0, 0, 1200, 900);
    w.__mediaDiagnostic.png = Uint8Array.from(
      atob(canvas.toDataURL("image/png").split(",")[1]),
      (c) => c.charCodeAt(0),
    ).buffer;
  });
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect(
    page.getByRole("progressbar", { name: "Receiving file…" }),
  ).toHaveAttribute("aria-valuenow", "50");
  const messageRow = page.locator(
    '[data-message-id="early-media-stable-message"]',
  );
  const receivingHeight = await messageRow.evaluate(
    (row) => row.getBoundingClientRect().height,
  );
  const initialCalls = await page.evaluate(
    () => (window as unknown as MediaWindow).__mediaDiagnostic.calls,
  );
  expect(initialCalls).toEqual([]);
  await expect(page.getByTestId("file-image")).toHaveCount(0);
  // Validate the bytes decode BEFORE making them available; no React state/event/navigation changes.
  expect(
    await page.evaluate(async () => {
      const d = (window as unknown as MediaWindow).__mediaDiagnostic;
      const bitmap = await createImageBitmap(
        new Blob([d.png], { type: "image/png" }),
      );
      const width = bitmap.width;
      bitmap.close();
      d.available = true;
      return width;
    }),
  ).toBe(1200);
  try {
    await expect
      .poll(
        () =>
          page
            .getByTestId("file-image")
            .evaluateAll((imgs) =>
              imgs.some((img) => (img as HTMLImageElement).naturalWidth > 0),
            ),
        {
          timeout: 4000,
          message:
            "Received image should decode after late bytes become available without remount, events or identity changes",
        },
      )
      .toBe(true);
    await expect
      .poll(() =>
        messageRow.evaluate((row) => row.getBoundingClientRect().height),
      )
      .toBe(receivingHeight);
    await page.getByRole("button", { name: "View full size" }).click();
    const lightbox = page.getByTestId("file-image-lightbox");
    await expect(lightbox).toBeVisible();
    const enlargedImage = lightbox.locator("img");
    await expect(enlargedImage).toBeVisible();
    await expect(enlargedImage).toHaveCSS("user-select", "text");
    await expect(page.getByTestId("file-image")).toHaveCSS(
      "user-select",
      "none",
    );
    await lightbox
      .getByRole("button", { name: "Zoom in", exact: true })
      .click();
    await enlargedImage.click({ button: "right" });
    const menu = page.getByRole("menu");
    await expect(menu).toBeVisible();
    await expect(
      menu.getByRole("menuitem", { name: "Copy image", exact: true }),
    ).toBeVisible();
    await expect(menu.getByText("Open Image in New Window")).toHaveCount(0);
    await expect(menu.getByText("Copy Subject")).toHaveCount(0);
    if (browserName === "chromium")
      await page.evaluate(() =>
        navigator.clipboard.writeText("before image copy"),
      );
    await menu
      .getByRole("menuitem", { name: "Copy image", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(() => {
          const d = (window as unknown as MediaWindow).__mediaDiagnostic;
          return { size: d.copiedImage, error: d.copyError };
        }),
      )
      .toEqual({ size: [1200, 900], error: null });
    if (browserName === "chromium") {
      const copied = await page.evaluate(async () => {
        const items = await navigator.clipboard.read();
        const png = await items
          .find((item) => item.types.includes("image/png"))!
          .getType("image/png");
        const bitmap = await createImageBitmap(png);
        const size = [bitmap.width, bitmap.height];
        bitmap.close();
        return size;
      });
      expect(copied).toEqual([1200, 900]);
    }
    await enlargedImage.click({ button: "right" });
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);
    await expect(lightbox).toBeVisible();
    await enlargedImage.click({ button: "right" });
    await menu.getByRole("menuitem", { name: "Save as…", exact: true }).click();
    await expect
      .poll(() =>
        page.evaluate(() =>
          (window as unknown as MediaWindow).__mediaDiagnostic.actions.some(
            (action) => action.cmd === "save_file",
          ),
        ),
      )
      .toBe(true);
    await enlargedImage.click({ button: "right" });
    await expect(
      menu.getByRole("menuitem", { name: "Reveal", exact: true }),
    ).toBeVisible();
    await menu.getByRole("menuitem", { name: "Reveal", exact: true }).click();
    await expect
      .poll(() =>
        page.evaluate(() =>
          (window as unknown as MediaWindow).__mediaDiagnostic.actions.some(
            (action) => action.cmd === "plugin:opener|reveal_item_in_dir",
          ),
        ),
      )
      .toBe(true);
    await lightbox.getByRole("button", { name: "Fit to window" }).click();
    // Releasing a text-selection drag on the backdrop must keep the preview open.
    await enlargedImage.hover();
    await page.mouse.down();
    await page.mouse.move(2, 2);
    await page.mouse.up();
    await expect(lightbox).toBeVisible();
    await lightbox
      .getByRole("button", { name: "Zoom in", exact: true })
      .click();
    const transform = lightbox.getByTestId("image-transform");
    await expect(transform).toHaveCSS("transform", /matrix\(1.25,/);
    const box = (await enlargedImage.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(
      box.x + box.width / 2 + 80,
      box.y + box.height / 2 + 60,
      { steps: 5 },
    );
    await page.mouse.up();
    await expect(lightbox).toBeVisible();
    expect(
      await transform.evaluate(
        (el) => new DOMMatrix(getComputedStyle(el).transform).m42,
      ),
    ).toBeGreaterThan(0);
    await lightbox.getByRole("button", { name: "Fit to window" }).click();
    await expect(transform).toHaveCSS("transform", "matrix(1, 0, 0, 1, 0, 0)");
    await lightbox.getByRole("button", { name: "Close" }).click();
    await expect(lightbox).toHaveCount(0);
    await expect(page.getByRole("menu")).toHaveCount(0);
    await page.getByRole("button", { name: "View full size" }).click();
    await expect(lightbox).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(lightbox).toHaveCount(0);
    await expect(page.getByTestId("file-image")).toBeVisible();
  } finally {
    console.log(
      "MEDIA_DIAGNOSTIC",
      JSON.stringify(
        await page.evaluate(() => {
          const d = (window as unknown as MediaWindow).__mediaDiagnostic;
          return {
            available: d.available,
            calls: d.calls,
            historyCalls: d.historyCalls,
            identity: d.identity,
            imageCount: document.querySelectorAll('[data-testid="file-image"]')
              .length,
          };
        }),
      ),
    );
  }
});
