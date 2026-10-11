import { test, expect } from "./helpers/portable-evidence";
import { enterChat, openBobDm, revealComposerTools } from "./helpers/session";

for (const attachment of [
  { kind: "file", path: "/tmp/large.bin", button: "composer-attach" },
  { kind: "image", path: "/tmp/photo.png", button: "composer-image" },
])
  test(`${attachment.kind} sender keeps confirmed upload percent and ETA visible until recipient custody`, async ({
    page,
  }, testInfo) => {
    await enterChat(page);
    await openBobDm(page);
    await page.evaluate((path) => {
      const win = window as unknown as Window & {
        __upload: { id: string; done: number; delivered: boolean };
        __TAURI_INTERNALS__: {
          invoke: (
            cmd: string,
            args: Record<string, unknown>,
          ) => Promise<unknown>;
        };
      };
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__upload = { id: "", done: 0, delivered: false };
      win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
        if (cmd === "plugin:dialog|open") return path;
        const result = await original(cmd, args);
        if (cmd === "owner_enqueue_file")
          win.__upload.id = (result as { id: string }).id;
        if (cmd === "owner_delivery_statuses")
          return (result as Array<{ id: string; status: string }>).map((row) =>
            row.id === win.__upload.id
              ? {
                  ...row,
                  status: win.__upload.delivered ? "delivered" : "awaiting",
                  file_progress: {
                    done: win.__upload.done,
                    total: 8,
                    delivered: win.__upload.delivered,
                  },
                }
              : row,
          );
        return result;
      };
    }, attachment.path);
    await revealComposerTools(page);
    await page.getByTestId(attachment.button).click();
    const bubble = page.getByTestId("message-bubble").last();
    await expect(
      bubble.getByRole("progressbar", { name: /Waiting for recipient/ }),
    ).toHaveAttribute("aria-valuenow", "0");
    await page.evaluate(
      () =>
        ((window as unknown as { __upload: { done: number } }).__upload.done =
          2),
    );
    await expect(bubble.getByText("25%", { exact: true })).toBeVisible();
    await page.evaluate(
      () =>
        ((window as unknown as { __upload: { done: number } }).__upload.done =
          4),
    );
    await expect(bubble.getByText(/left/)).toBeVisible();
    const halfProgressImage = testInfo.outputPath("upload-half.png");
    await page.screenshot({ path: halfProgressImage });
    await testInfo.attach("upload-at-half", {
      path: halfProgressImage,
      contentType: "image/png",
    });
    await page.evaluate(() => {
      const state = (
        window as unknown as { __upload: { done: number; delivered: boolean } }
      ).__upload;
      state.done = 8;
    });
    await expect(
      bubble.getByRole("progressbar", { name: /Confirming receipt/ }),
    ).toHaveAttribute("aria-valuenow", "99");
    await page.evaluate(() => {
      const state = (window as unknown as { __upload: { delivered: boolean } })
        .__upload;
      state.delivered = true;
    });
    await expect(
      bubble.getByRole("progressbar", { name: /Sending/ }),
    ).toHaveCount(0);
  });

test("concurrent sends keep progress with their own file bubbles", async ({
  page,
}) => {
  await enterChat(page);
  await openBobDm(page);
  await page.evaluate(() => {
    const win = window as unknown as Window & {
      __sendProgress: {
        requests: Array<{ key: string; release: () => void }>;
        emit: (key: string, done: number, total?: number) => void;
      };
      __mockEmit: (name: string, payload: unknown) => void;
      __TAURI_INTERNALS__: {
        invoke: (
          cmd: string,
          args: Record<string, unknown>,
        ) => Promise<unknown>;
      };
    };
    let picked = 0;
    const original = win.__TAURI_INTERNALS__.invoke;
    const requests: Array<{ key: string; release: () => void }> = [];
    win.__sendProgress = {
      requests,
      emit: (key, done, total = 4) =>
        win.__mockEmit("file-progress", {
          file_conv: key,
          direction: "prepare",
          done,
          total,
        }),
    };
    win.__TAURI_INTERNALS__.invoke = (cmd, args) => {
      if (cmd === "plugin:dialog|open")
        return Promise.resolve(`/tmp/${++picked === 1 ? "alpha" : "beta"}.bin`);
      if (cmd === "owner_enqueue_file")
        return new Promise((resolve, reject) => {
          requests.push({
            key: String(args.progressKey),
            release: () => void original(cmd, args).then(resolve, reject),
          });
        });
      return original(cmd, args);
    };
  });

  await revealComposerTools(page);
  await page.getByTestId("composer-attach").click();
  await revealComposerTools(page);
  await page.getByTestId("composer-attach").click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { __sendProgress: { requests: unknown[] } })
            .__sendProgress.requests.length,
      ),
    )
    .toBe(2);
  const keys = await page.evaluate(() =>
    (
      window as unknown as {
        __sendProgress: { requests: Array<{ key: string }> };
      }
    ).__sendProgress.requests.map((r) => r.key),
  );
  expect(keys[0]).not.toBe(keys[1]);
  await page.evaluate(([first, second]) => {
    const state = (
      window as unknown as {
        __sendProgress: {
          emit: (key: string, done: number, total?: number) => void;
        };
      }
    ).__sendProgress;
    state.emit(first, 1);
    state.emit(second, 2);
  }, keys);
  await expect(
    page
      .getByTestId("message-bubble")
      .filter({ hasText: "alpha.bin" })
      .getByText("25%", { exact: true }),
  ).toBeVisible();
  await expect(
    page
      .getByTestId("message-bubble")
      .filter({ hasText: "beta.bin" })
      .getByText("50%", { exact: true }),
  ).toBeVisible();
  await page.evaluate(([first]) => {
    (
      window as unknown as {
        __sendProgress: {
          emit: (key: string, done: number, total?: number) => void;
        };
      }
    ).__sendProgress.emit(first, 200, 201);
  }, keys);
  const firstBar = page
    .getByTestId("message-bubble")
    .filter({ hasText: "alpha.bin" })
    .getByRole("progressbar", { name: /Preparing/ });
  await expect(firstBar).toHaveAttribute("aria-valuenow", "99");
  await expect(firstBar).toHaveAttribute("aria-valuemin", "0");
  await expect(firstBar).toHaveAttribute("aria-valuemax", "100");
  await expect(
    page
      .getByTestId("message-bubble")
      .filter({ hasText: "alpha.bin" })
      .getByText("99%", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    for (const request of (
      window as unknown as {
        __sendProgress: { requests: Array<{ release: () => void }> };
      }
    ).__sendProgress.requests)
      request.release();
  });
});
