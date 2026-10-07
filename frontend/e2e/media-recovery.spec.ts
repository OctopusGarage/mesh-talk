import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB } from "./helpers/session";

type Diagnostic = {
  available: boolean;
  calls: Array<{ cmd: string; available: boolean; rejected: boolean }>;
  identity: string;
  historyCalls: number;
  png: ArrayBuffer;
};
type MediaWindow = Window & {
  __mediaDiagnostic: Diagnostic;
  __TAURI_INTERNALS__: {
    invoke: (cmd: string, args: Record<string, unknown>) => Promise<unknown>;
  };
};

test("received media recovers when bytes become available with stable fileConv", async ({
  page,
}) => {
  await enterChat(page);
  await page.evaluate(() => {
    const w = window as unknown as MediaWindow;
    w.__mediaDiagnostic = {
      available: false,
      calls: [],
      identity: "fc_early_media_diagnostic",
      historyCalls: 0,
      png: new ArrayBuffer(0),
    };
    const original = w.__TAURI_INTERNALS__.invoke;
    w.__TAURI_INTERNALS__.invoke = async (
      cmd: string,
      args: Record<string, unknown>,
    ) => {
      const d = w.__mediaDiagnostic;
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
    canvas.width = 2;
    canvas.height = 2;
    canvas.getContext("2d")!.fillRect(0, 0, 2, 2);
    w.__mediaDiagnostic.png = Uint8Array.from(
      atob(canvas.toDataURL("image/png").split(",")[1]),
      (c) => c.charCodeAt(0),
    ).buffer;
  });
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  await expect(page.getByTestId("conversation-header")).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as MediaWindow).__mediaDiagnostic.calls.filter(
            (c) => c.cmd === "read_file",
          ).length,
      ),
    )
    .toBeGreaterThanOrEqual(1);
  const initialCalls = await page.evaluate(
    () => (window as unknown as MediaWindow).__mediaDiagnostic.calls,
  );
  expect(initialCalls.some((c) => c.cmd === "read_media")).toBe(true);
  expect(initialCalls.some((c) => c.cmd === "read_file")).toBe(true);
  expect(initialCalls.every((c) => c.rejected && !c.available)).toBe(true);
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
  ).toBe(2);
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
