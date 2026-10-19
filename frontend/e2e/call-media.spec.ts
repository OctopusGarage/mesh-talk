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
test("real call engine exchanges encrypted audio/video and releases capture on hangup", async ({
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
        ({ payload }) => window.__callMediaHarness.signal("device-a", payload),
        signal,
      );
    });
    await b.exposeFunction("__deliverCallSignal", async (signal: Signal) => {
      expect(signal.target).toBe("device-a");
      await a.evaluate(
        ({ payload }) => window.__callMediaHarness.signal("device-b", payload),
        signal,
      );
    });
    await Promise.all([
      prepareCallMedia(a, baseURL!, "device-a"),
      prepareCallMedia(b, baseURL!, "device-b"),
    ]);
    await a.evaluate(() => window.__callMediaHarness.start("device-b"));
    await expect
      .poll(() => b.evaluate(() => window.__callMediaHarness.phase()))
      .toBe("incoming");
    await b.evaluate(() => window.__callMediaHarness.accept());
    for (const page of [a, b]) {
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
              [
                "audioSent",
                "audioReceived",
                "videoSent",
                "videoReceived",
              ].every((key) => Number(stats[key]) > 0)
            );
          },
          { timeout: 10_000 },
        )
        .toBe(true);
    }
    await a.evaluate(() => window.__callMediaHarness.hangup());
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
