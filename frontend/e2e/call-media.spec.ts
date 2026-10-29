import { test, expect } from "@playwright/test";

import { prepareCallMedia } from "./helpers/call-media-harness";

// Production call engine and real Chromium capture/ICE/DTLS-SRTP. The backend
// signaling boundary is substituted; this does not prove native/Tailscale media.
test.use({
  launchOptions: {
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
    ],
  },
});

type Signal = { target: string; payload: string };
for (const scenario of [
  { name: "video", video: true, cameras: [true, true], remoteVideo: [1, 1] },
  { name: "voice", video: false, cameras: [true, true], remoteVideo: [0, 0] },
  {
    name: "video with audio-only fallback",
    video: true,
    cameras: [false, false],
    remoteVideo: [0, 0],
  },
  {
    name: "video with no caller camera",
    video: true,
    cameras: [false, true],
    remoteVideo: [0, 0],
  },
  {
    name: "video with no callee camera",
    video: true,
    cameras: [true, false],
    remoteVideo: [0, 1],
  },
]) {
  test(`real ${scenario.name} call plays remote media and releases capture on hangup`, async ({
    browser,
    baseURL,
  }) => {
    const contextA = await browser.newContext({
      permissions: ["microphone", "camera"],
    });
    const contextB = await browser.newContext({
      permissions: ["microphone", "camera"],
    });
    const a = await contextA.newPage();
    const b = await contextB.newPage();
    try {
      await a.exposeFunction("__deliverCallSignal", async (signal: Signal) => {
        expect(signal.target).toBe("device-b");
        await b.evaluate(
          ({ payload }) =>
            window.__callMediaHarness.signal("device-a", payload),
          signal,
        );
      });
      await b.exposeFunction("__deliverCallSignal", async (signal: Signal) => {
        expect(signal.target).toBe("device-a");
        await a.evaluate(
          ({ payload }) =>
            window.__callMediaHarness.signal("device-b", payload),
          signal,
        );
      });
      await Promise.all([
        prepareCallMedia(a, baseURL!, "device-a", false, !scenario.cameras[0]),
        prepareCallMedia(b, baseURL!, "device-b", false, !scenario.cameras[1]),
      ]);
      await a.evaluate(
        (video) => window.__callMediaHarness.start("device-b", video),
        scenario.video,
      );
      await expect
        .poll(() => b.evaluate(() => window.__callMediaHarness.phase()))
        .toBe("incoming");
      await b.getByTestId("call-accept").click();
      for (const [index, page] of [a, b].entries()) {
        await expect
          .poll(() => page.evaluate(() => window.__callMediaHarness.phase()))
          .toBe("connected");
        await expect
          .poll(
            async () => {
              const stats = await page.evaluate(() =>
                window.__callMediaHarness.stats(),
              );
              return (
                stats.dtls === true &&
                Number(stats.audioSamples) > 0 &&
                (!scenario.remoteVideo[index] ||
                  Number(stats.videoFrames) > 0) &&
                [
                  "audioSent",
                  "audioReceived",
                  ...(scenario.remoteVideo[index] ? ["videoReceived"] : []),
                  ...(scenario.remoteVideo[1 - index] ? ["videoSent"] : []),
                ].every((key) => Number(stats[key]) > 0)
              );
            },
            { timeout: 10_000 },
          )
          .toBe(true);
        await expect
          .poll(() => page.evaluate(() => window.__callMediaHarness.playback()))
          .toEqual({
            playing: true,
            players: 1,
            videoTracks: scenario.remoteVideo[index],
          });
      }
      await a.getByTestId("call-hangup").click();
      for (const page of [a, b]) {
        await expect
          .poll(() => page.evaluate(() => window.__callMediaHarness.phase()))
          .toBe("idle");
        await expect
          .poll(() => page.evaluate(() => window.__callMediaHarness.cleaned()))
          .toBe(true);
      }
    } finally {
      await Promise.all([contextA.close(), contextB.close()]);
    }
  });
}
