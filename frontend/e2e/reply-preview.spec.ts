import { test, expect } from "./helpers/portable-evidence";
import { enterChat, BOB } from "./helpers/session";

for (const theme of ["light", "dark"] as const) {
  test(`photo quotes identify their source and navigate virtualized history (${theme})`, async ({
    page,
  }, testInfo) => {
    await enterChat(page);
    await page.evaluate(
      ({ account, theme }) => {
        document.documentElement.classList.toggle("dark", theme === "dark");
        const canvas = document.createElement("canvas");
        canvas.width = 1600;
        canvas.height = 200;
        const ctx = canvas.getContext("2d")!;
        ctx.fillStyle = "#197c87";
        ctx.fillRect(0, 0, 800, 200);
        ctx.fillStyle = "#e7b46f";
        ctx.fillRect(800, 0, 800, 200);
        const png = Uint8Array.from(
          atob(canvas.toDataURL().split(",")[1]),
          (c) => c.charCodeAt(0),
        ).buffer;
        const w = window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              cmd: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        };
        const invoke = w.__TAURI_INTERNALS__.invoke;
        w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          if (
            (cmd === "read_media" || cmd === "read_file") &&
            args.fileConv === "quote-photo"
          )
            return png;
          if (cmd === "file_statuses")
            return (args.fileConvs as string[]).map((file_conv) => ({
              file_conv,
              ready: true,
              done: 1,
              total: 1,
            }));
          if (cmd === "owner_account_history" && args.account === account) {
            const base = {
              from_me: false,
              who: "device_bob_2222",
              wall_clock: Date.now() - 100_000,
              reply_to: null,
              text: "",
            };
            return [
              {
                ...base,
                id: "photo-original",
                file: {
                  file_conv: "quote-photo",
                  name: "image.png",
                  mime: "image/png",
                  size: 500,
                  media: true,
                },
              },
              ...Array.from({ length: 60 }, (_, i) => ({
                ...base,
                id: `between-${i}`,
                text: `History message ${i}`,
                wall_clock: base.wall_clock + i + 1,
              })),
              {
                ...base,
                id: "photo-reply",
                from_me: true,
                reply_to: "photo-original",
                text: "This is the photo I meant",
                wall_clock: Date.now(),
              },
            ];
          }
          return invoke(cmd, args);
        };
      },
      { account: BOB.account, theme },
    );
    await page.getByTestId(`conversation-row-${BOB.account}`).click();
    const quote = page.getByTestId("message-parent-snippet");
    await expect(quote).toHaveRole("button");
    await expect(quote).toContainText("bob");
    await expect(quote).not.toContainText("image.png");
    // Quoted context follows the reply, outside the primary bubble.
    await expect(quote.locator("xpath=preceding-sibling::*[1]")).toContainText(
      "This is the photo I meant",
    );
    await expect
      .poll(() =>
        quote
          .locator("img")
          .evaluate((img: HTMLImageElement) => img.naturalWidth),
      )
      .toBe(1600);
    const measurement = {
      current: null as {
        thumbnail: { width: number; height: number };
        thumbnailFrame: { height: number };
      } | null,
    };
    await expect
      .poll(async () => {
        // Virtualization hides rows while measuring. Read both rectangles from
        // one visible, decoded instance rather than across separate remounts.
        measurement.current = await quote
          .locator("img")
          .evaluate((img: HTMLImageElement) => {
            const thumbnail = img.getBoundingClientRect();
            const thumbnailFrame = img.parentElement!.getBoundingClientRect();
            if (
              getComputedStyle(img).visibility !== "visible" ||
              img.naturalWidth !== 1600 ||
              thumbnail.width === 0 ||
              thumbnail.height === 0 ||
              thumbnailFrame.height === 0
            )
              return null;
            return {
              thumbnail: { width: thumbnail.width, height: thumbnail.height },
              thumbnailFrame: { height: thumbnailFrame.height },
            };
          });
        return measurement.current;
      })
      .not.toBeNull();
    const { thumbnail, thumbnailFrame } = measurement.current!;
    expect(thumbnail.width / thumbnail.height).toBeCloseTo(8, 1);
    expect(thumbnail.width).toBeLessThanOrEqual(64);
    expect(thumbnailFrame.height).toBeCloseTo(thumbnail.height, 0);
    await expect(
      page.locator('[data-message-id="photo-original"]'),
    ).toHaveCount(0);
    await expect(quote).toBeInViewport();
    await quote.screenshot({ path: testInfo.outputPath(`quote-${theme}.png`) });
    await quote.click();
    const original = page.locator('[data-message-id="photo-original"]');
    await expect(original).toBeVisible();
    await expect(original).toHaveClass(/bg-signal\/10/);
    await expect(page.getByTestId("file-image-lightbox")).toHaveCount(0);
    // The source photo keeps its ordinary enlarge interaction.
    await original.getByRole("button", { name: "View full size" }).click();
    await expect(page.getByTestId("file-image-lightbox")).toBeVisible();
    await page
      .getByTestId("file-image-lightbox")
      .getByRole("button", { name: "Close", exact: true })
      .click();
    await original.hover();
    await original.getByTestId("message-reply").click();
    const banner = page.getByTestId("composer-reply-banner");
    await expect(banner.locator("img")).toHaveAttribute("alt", "image.png");
    await page.getByRole("button", { name: "Cancel reply" }).click();
    await page.getByTestId("jump-to-latest").click();
    await expect(quote).toBeVisible();
    await page.setViewportSize({ width: 760, height: 620 });
    await expect(quote).toBeInViewport();
    await quote.screenshot({
      path: testInfo.outputPath(`quote-${theme}-detail.png`),
    });
    await page.screenshot({
      path: testInfo.outputPath(`quote-${theme}-narrow.png`),
    });
    await expect(quote).toBeVisible();
  });
}

for (const available of [true, false]) {
  test(`older reply sources ${available ? "open exact context" : "explain missing history"}`, async ({
    page,
  }) => {
    await enterChat(page);
    await page.evaluate(
      ({ account, available }) => {
        const w = window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              cmd: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        };
        const invoke = w.__TAURI_INTERNALS__.invoke;
        const base = {
          from_me: false,
          who: "device_bob_2222",
          wall_clock: Date.now() - 1000,
          reply_to: null,
          text: "",
        };
        w.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          if (cmd === "owner_account_history" && args.account === account)
            return [
              {
                ...base,
                id: "new-reply",
                text: "A reply to older history",
                reply_to: "older-original",
              },
            ];
          if (cmd === "owner_search_context")
            return {
              status: available ? "found" : "unavailable",
              reference: args.reference,
              messages: available
                ? [
                    {
                      ...base,
                      id: "older-original",
                      text: "The exact original",
                    },
                  ]
                : [],
              newer_entries: 900,
            };
          return invoke(cmd, args);
        };
      },
      { account: BOB.account, available },
    );
    await page.getByTestId(`conversation-row-${BOB.account}`).click();
    const quote = page.getByTestId("message-parent-snippet");
    await expect(quote).toHaveText("View original");
    await quote.click();
    if (available) {
      const original = page.locator('[data-message-id="older-original"]');
      await expect(original).toBeVisible();
      await expect(original).toHaveClass(/bg-signal\/10/);
      await page
        .getByRole("button", { name: "Jump to latest messages", exact: true })
        .first()
        .click();
      await expect(page.getByRole("log")).toContainText(
        "A reply to older history",
      );
    } else {
      await expect(
        page
          .getByRole("status")
          .filter({ hasText: "This message is no longer available." }),
      ).toBeVisible();
      await expect(quote).toBeEnabled();
      await expect(page.getByRole("log")).toContainText(
        "A reply to older history",
      );
    }
  });
}
