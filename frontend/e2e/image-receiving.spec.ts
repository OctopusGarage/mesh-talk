import type { Page } from "@playwright/test";
import { test, expect } from "./helpers/portable-evidence";
import { BOB, enterChat, seedThemeBeforeLoad } from "./helpers/session";

type ReceivingWindow = Window & {
  __imageReceiving: {
    done: number;
    ready: boolean;
    releaseRead: (() => void) | null;
    reads: number;
  };
  __TAURI_INTERNALS__: {
    invoke: (
      command: string,
      args: Record<string, unknown>,
    ) => Promise<unknown>;
  };
};

async function seedImage(page: Page, done = 0, delayedDecode = false) {
  await enterChat(page);
  const png = await page.evaluate(
    ({ account, device, done, delayedDecode }) => {
      const win = window as unknown as ReceivingWindow;
      win.__imageReceiving = {
        done,
        ready: false,
        releaseRead: null,
        reads: 0,
      };
      const canvas = document.createElement("canvas");
      canvas.width = 800;
      canvas.height = 600;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#d3e9df";
      context.fillRect(0, 0, 800, 600);
      context.fillStyle = "#356b60";
      context.fillRect(80, 80, 640, 440);
      const base64 = canvas.toDataURL("image/png").split(",")[1];
      const bytes = Uint8Array.from(atob(base64), (char) =>
        char.charCodeAt(0),
      ).buffer;
      const original = win.__TAURI_INTERNALS__.invoke;
      win.__TAURI_INTERNALS__.invoke = async (command, args) => {
        const probe = win.__imageReceiving;
        if (command === "file_statuses")
          return (args.fileConvs as string[]).map((file_conv) => ({
            file_conv,
            done: probe.done,
            total: 4,
            ready: probe.ready,
          }));
        if (command === "read_media" && args.fileConv === "receiving-image") {
          probe.reads++;
          await new Promise<void>((resolve) => {
            probe.releaseRead = resolve;
          });
          return bytes;
        }
        if (command === "owner_account_history" && args.account === account)
          return [
            {
              id: "receiving-message",
              from_me: false,
              who: device,
              text: "",
              wall_clock: 1700000000000,
              reply_to: null,
              file: {
                name: "receipt.png",
                size: bytes.byteLength,
                mime: "image/png",
                file_conv: "receiving-image",
                media: true,
              },
            },
          ];
        return original(command, args);
      };
      if (delayedDecode) {
        // Keep the real <img> load path pending after the media read has completed.
        // This fixture URL is fulfilled with the same PNG; production uses Blob URLs.
        const createObjectURL = URL.createObjectURL.bind(URL);
        URL.createObjectURL = (value) =>
          value instanceof Blob && value.type === "image/png"
            ? `${location.origin}/receiving-fixture.png`
            : createObjectURL(value);
      }
      return base64;
    },
    { ...BOB, done, delayedDecode },
  );
  await page.getByTestId(`conversation-row-${BOB.account}`).click();
  return { row: page.locator('[data-message-id="receiving-message"]'), png };
}

test("image receipt preserves its frame through chunks, local read and image loading", async ({
  page,
}) => {
  let releaseImage!: () => void;
  let requestImage!: () => void;
  const requested = new Promise<void>((resolve) => {
    requestImage = resolve;
  });
  const release = new Promise<void>((resolve) => {
    releaseImage = resolve;
  });
  let png = "";
  await page.route("**/receiving-fixture.png", async (route) => {
    requestImage();
    await release;
    await route.fulfill({
      contentType: "image/png",
      body: Buffer.from(png, "base64"),
    });
  });
  try {
    const seeded = await seedImage(page, 0, true);
    png = seeded.png;
    const row = seeded.row;
    const initial = await row.boundingBox();
    await expect(row.getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "0",
    );
    await expect(
      row.getByRole("paragraph").filter({ hasText: /^Waiting for image$/ }),
    ).toBeVisible();
    await expect(row.getByText("0%", { exact: true })).toHaveCount(0);
    expect(
      await page.evaluate(
        () => (window as unknown as ReceivingWindow).__imageReceiving.reads,
      ),
    ).toBe(0);
    await page.evaluate(() => {
      (window as unknown as ReceivingWindow).__imageReceiving.done = 3;
    });
    await expect(row.getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "75",
    );
    await expect(row.getByText("Receiving image · 75%")).toBeVisible();
    await page.evaluate(() => {
      (window as unknown as ReceivingWindow).__imageReceiving.done = 4;
    });
    await expect(
      row.getByRole("paragraph").filter({ hasText: /^Preparing image$/ }),
    ).toBeVisible();
    await expect(row.getByRole("progressbar")).toHaveCount(0);
    await page.evaluate(() => {
      (window as unknown as ReceivingWindow).__imageReceiving.ready = true;
    });
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as ReceivingWindow).__imageReceiving.reads,
        ),
      )
      .toBe(1);
    await expect(row.getByTestId("file-image")).toHaveCount(0);
    await page.evaluate(() =>
      (window as unknown as ReceivingWindow).__imageReceiving.releaseRead!(),
    );
    await requested;
    const image = row.getByTestId("file-image");
    await expect(image).toHaveCSS("opacity", "0");
    await expect(
      row.getByRole("button", { name: "View full size" }),
    ).toBeDisabled();
    await expect(
      row.getByRole("paragraph").filter({ hasText: /^Preparing image$/ }),
    ).toBeVisible();
    releaseImage();
    await expect
      .poll(() =>
        image.evaluate((element) => (element as HTMLImageElement).naturalWidth),
      )
      .toBe(800);
    await expect(image).toHaveCSS("opacity", "1");
    await expect(
      row.getByRole("paragraph").filter({ hasText: /^Preparing image$/ }),
    ).toHaveCount(0);
    await expect(
      row.getByRole("button", { name: "View full size" }),
    ).toBeEnabled();
    const final = await row.boundingBox();
    expect(Math.abs(final!.height - initial!.height)).toBeLessThanOrEqual(1);
    await image.click();
    await expect(page.getByTestId("file-image-lightbox")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(
      row.getByRole("button", { name: "View full size" }),
    ).toBeFocused();
  } finally {
    releaseImage();
  }
});

for (const theme of ["light", "dark", "oled", "cat-acrylic"]) {
  test(`image progress stays legible and bounded in ${theme}`, async ({
    page,
  }, testInfo) => {
    await seedThemeBeforeLoad(page, theme);
    await page.setViewportSize({
      width: theme === "dark" ? 760 : 1280,
      height: 800,
    });
    const { row } = await seedImage(page, 3);
    const progress = row.getByRole("progressbar");
    await expect(progress).toHaveAttribute("aria-valuenow", "75");
    const frame = row.locator('[title="receipt.png"]');
    const dimensions = await frame.boundingBox();
    expect(dimensions!.width).toBeLessThanOrEqual(320);
    expect(dimensions!.width / dimensions!.height).toBeCloseTo(4 / 3, 1);
    const contrast = await frame.evaluate((element) => {
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = 1;
      const ctx = canvas.getContext("2d")!;
      const color = (value: string) => {
        ctx.clearRect(0, 0, 1, 1);
        ctx.fillStyle = value;
        ctx.fillRect(0, 0, 1, 1);
        return Array.from(ctx.getImageData(0, 0, 1, 1).data);
      };
      const luminance = (rgb: number[]) =>
        rgb
          .slice(0, 3)
          .map((value) => {
            const channel = value / 255;
            return channel <= 0.04045
              ? channel / 12.92
              : ((channel + 0.055) / 1.055) ** 2.4;
          })
          .reduce(
            (sum, channel, i) => sum + channel * [0.2126, 0.7152, 0.0722][i],
            0,
          );
      const background = color(getComputedStyle(element).backgroundColor);
      const against = (foreground: number[]) => {
        const a = luminance(foreground),
          b = luminance(background);
        return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
      };
      return {
        alpha: background[3],
        text: Array.from(element.querySelectorAll("p")).map((text) =>
          against(color(getComputedStyle(text).color)),
        ),
        ring: against(
          color(
            getComputedStyle(element.querySelector("circle:last-of-type")!)
              .color,
          ),
        ),
        overflow: element.scrollWidth > element.clientWidth,
      };
    });
    expect(contrast.alpha).toBe(255);
    expect(contrast.text.every((ratio) => ratio >= 4.5)).toBe(true);
    expect(contrast.ring).toBeGreaterThanOrEqual(3);
    expect(contrast.overflow).toBe(false);
    await page.emulateMedia({ reducedMotion: "reduce" });
    const ring = progress.locator("circle").last();
    await expect
      .poll(() =>
        ring.evaluate(
          (element) => getComputedStyle(element).transitionProperty,
        ),
      )
      .not.toContain("stroke-dashoffset");
    await testInfo.attach(`image-receiving-${theme}`, {
      body: await row.screenshot(),
      contentType: "image/png",
    });
  });
}
