import { readFileSync } from "node:fs";
import { test, expect } from "./helpers/portable-evidence";
import { BOB, CHANNEL, enterChat, openBobDm } from "./helpers/session";

const clips = (
  JSON.parse(
    readFileSync(
      new URL("./fixtures/video-metadata.json", import.meta.url),
      "utf8",
    ),
  ) as {
    clips: Array<{
      width: number;
      height: number;
      base64: string;
      mime: string;
    }>;
  }
).clips;
async function holdPlayerSource(page: import("@playwright/test").Page) {
  await page.evaluate(() => {
    const original = HTMLMediaElement.prototype.setAttribute;
    const held: Array<{ node: HTMLMediaElement; src: string }> = [];
    HTMLMediaElement.prototype.setAttribute = function (name, value) {
      if (name === "src" && this.tagName === "VIDEO") {
        held.push({ node: this, src: value });
        return;
      }
      return original.call(this, name, value);
    };
    Object.assign(window, {
      __releasePlayer: () => {
        HTMLMediaElement.prototype.setAttribute = original;
        for (const { node, src } of held) original.call(node, "src", src);
      },
    });
  });
}

for (const { width, height, viewport } of [
  { width: 640, height: 360, viewport: { width: 1040, height: 720 } },
  { width: 1920, height: 1080, viewport: { width: 760, height: 520 } },
  { width: 1080, height: 1920, viewport: { width: 760, height: 520 } },
]) {
  test(`video keeps ${width}x${height} geometry while metadata loads`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await enterChat(page);
    await page.evaluate(
      async ({ account, device, encoded }) => {
        const bytes = Uint8Array.from(atob(encoded), (value) =>
          value.charCodeAt(0),
        ).buffer;
        const win = window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (
              cmd: string,
              args: Record<string, unknown>,
            ) => Promise<unknown>;
          };
        };
        const original = win.__TAURI_INTERNALS__.invoke;
        win.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
          if (cmd === "read_media" && args.fileConv === "video-layout")
            return bytes;
          if (cmd === "file_statuses")
            return (args.fileConvs as string[]).map((file_conv) => ({
              file_conv,
              ready: true,
              done: 1,
              total: 1,
            }));
          const value = await original(cmd, args);
          if (cmd === "owner_account_history" && args.account === account)
            return [
              ...(value as unknown[]),
              {
                id: "video-layout",
                from_me: false,
                who: device,
                text: "",
                wall_clock: Date.now(),
                reply_to: null,
                file: {
                  file_conv: "video-layout",
                  name: "clip.webm",
                  size: bytes.byteLength,
                  media: true,
                  mime: "video/webm",
                },
              },
            ];
          return value;
        };
      },
      {
        ...BOB,
        encoded: clips.find(
          (clip) => clip.width === width && clip.height === height,
        )!.base64,
      },
    );
    await openBobDm(page);
    await expect(page.getByTestId("file-video")).toHaveJSProperty(
      "naturalWidth",
      Math.round(width * Math.min(1, 1280 / Math.max(width, height))),
    );
    await holdPlayerSource(page);
    await page.getByTestId("file-video").click();
    const player = page.getByTestId("file-video-player");
    await expect(player).toBeVisible();
    // offset dimensions exclude the shared-element projection transform.
    const before = await player.evaluate((el) => ({
      width: (el as HTMLVideoElement).offsetWidth,
      height: (el as HTMLVideoElement).offsetHeight,
      ready: (el as HTMLVideoElement).readyState,
    }));
    expect(before.ready).toBe(0);
    await page.evaluate(() =>
      (window as unknown as { __releasePlayer: () => void }).__releasePlayer(),
    );
    await expect(player).toHaveJSProperty("videoWidth", width);
    const after = await player.evaluate((el) => ({
      width: (el as HTMLVideoElement).offsetWidth,
      height: (el as HTMLVideoElement).offsetHeight,
    }));
    expect(before).toMatchObject(after);
    expect(after.width).toBeLessThanOrEqual(viewport.width - 48);
    expect(after.height).toBeLessThanOrEqual(viewport.height - 48);
    expect(after.width / after.height).toBeCloseTo(width / height, 1);
    await page.keyboard.press("Escape");
    await expect(player).not.toBeVisible();
    await page.getByTestId(`conversation-row-${CHANNEL.id}`).click();
    await openBobDm(page);
    await expect(page.getByTestId("file-video")).toHaveJSProperty(
      "naturalWidth",
      Math.round(width * Math.min(1, 1280 / Math.max(width, height))),
    );
    await holdPlayerSource(page);
    await page.getByTestId("file-video").click();
    const cached = await player.evaluate((el) => ({
      width: (el as HTMLVideoElement).offsetWidth,
      height: (el as HTMLVideoElement).offsetHeight,
      ready: (el as HTMLVideoElement).readyState,
    }));
    expect(cached).toMatchObject({ ...after, ready: 0 });
    await page.evaluate(() =>
      (window as unknown as { __releasePlayer: () => void }).__releasePlayer(),
    );
    await expect(player).toHaveJSProperty("videoWidth", width);
  });
}
