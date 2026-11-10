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
declare global {
  interface Window {
    __beforeCallAnswer: () => Promise<void>;
    __callIceGate: {
      ready: Promise<void>;
      release: () => void;
      listeners: () => number;
    };
  }
}
for (const scenario of [
  { name: "video", video: true, cameras: [true, true], remoteVideo: [1, 1] },
  {
    name: "video with deferred native answer application",
    video: true,
    cameras: [true, true],
    remoteVideo: [1, 1],
    deferredAnswer: true,
  },
  {
    name: "video with deferred native ICE readiness",
    video: true,
    cameras: [true, true],
    remoteVideo: [1, 1],
    deferredIce: true,
  },
  {
    name: "video terminated during native ICE preparation",
    video: true,
    cameras: [true, true],
    remoteVideo: [1, 1],
    deferredIce: true,
    terminalBeforeConnected: true,
  },
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
    let releaseAnswer = () => {};
    let answerDelay: ReturnType<typeof setTimeout> | undefined;
    try {
      const answers: Signal[] = [];
      if ("deferredIce" in scenario) {
        await b.clock.install();
        await b.addInitScript({
          content: `
          const NativeConnection = window.RTCPeerConnection;
          const nativeState = Object.getOwnPropertyDescriptor(NativeConnection.prototype, 'iceGatheringState').get;
          let held = true, nativeComplete, observerEntered;
          const completed = new Promise(resolve => { nativeComplete = resolve; });
          const observing = new Promise(resolve => { observerEntered = resolve; });
          const pending = new Map(), nativeEvents = new WeakMap();
          window.__callIceGate = {
            ready: Promise.all([completed, observing]).then(() => {}),
            listeners: () => pending.size,
            release() {
              held = false;
              for (const [listener, entry] of [...pending]) {
                if (entry.event) {
                  entry.event = null;
                  listener.call(entry.connection, entry.completeEvent);
                }
              }
            },
          };
          window.RTCPeerConnection = class extends NativeConnection {
            constructor(options) {
              super(options);
              super.addEventListener('icegatheringstatechange', event => {
                if (nativeState.call(this) === 'complete') { nativeEvents.set(this, event); nativeComplete(); }
              });
            }
            get iceGatheringState() { return held ? 'gathering' : nativeState.call(this); }
            get localDescription() {
              const description = super.localDescription;
              if (!held || !description) return description;
              return new RTCSessionDescription({ type: description.type, sdp: description.sdp.split('\\r\\n')
                .filter(line => !line.startsWith('a=candidate:') && line !== 'a=end-of-candidates').join('\\r\\n') });
            }
            addEventListener(type, listener, options) {
              if (type !== 'icegatheringstatechange') return super.addEventListener(type, listener, options);
              const entry = { connection: this, event: null, completeEvent: null, wrapped: null };
              entry.wrapped = event => {
                if (held && nativeState.call(this) === 'complete') {
                  entry.event = event; entry.completeEvent = event;
                } else listener.call(this, event);
              };
              if (nativeEvents.has(this)) entry.event = entry.completeEvent = nativeEvents.get(this);
              pending.set(listener, entry);
              observerEntered();
              return super.addEventListener(type, entry.wrapped, options);
            }
            removeEventListener(type, listener, options) {
              const entry = type === 'icegatheringstatechange' && pending.get(listener);
              if (entry) { pending.delete(listener); return super.removeEventListener(type, entry.wrapped, options); }
              return super.removeEventListener(type, listener, options);
            }
          };
        `,
        });
      }
      let answerObserved = Promise.resolve();
      if ("deferredAnswer" in scenario) {
        let observeAnswer = () => {};
        answerObserved = new Promise<void>((resolve) => {
          observeAnswer = resolve;
        });
        const answerReady = new Promise<void>((resolve) => {
          releaseAnswer = resolve;
        });
        await a.exposeFunction("__beforeCallAnswer", async () => {
          observeAnswer();
          await answerReady;
        });
        await a.addInitScript(() => {
          const apply = RTCPeerConnection.prototype.setRemoteDescription;
          RTCPeerConnection.prototype.setRemoteDescription = async function (
            description,
          ) {
            if (description?.type === "answer")
              await window.__beforeCallAnswer();
            return Reflect.apply(apply, this, [description]);
          };
        });
      }
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
        if (JSON.parse(signal.payload).kind === "answer") answers.push(signal);
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
      if ("deferredIce" in scenario) {
        await b.evaluate(() => window.__callIceGate.ready);
        await b.clock.runFor(2_001);
        // A non-trickle answer must not escape before actual native readiness.
        expect(answers.length).toBe(0);
        if ("terminalBeforeConnected" in scenario) {
          const outcome = b.evaluate(async () => {
            try {
              await window.__callMediaHarness.waitForConnection();
              return "connected";
            } catch {
              return "ended";
            }
          });
          await expect
            .poll(() =>
              b.evaluate(() => window.__callMediaHarness.connectionWaiters()),
            )
            .toBe(1);
          await b.clock.resume();
          await b.getByTestId("call-hangup").click();
          expect(await outcome).toBe("ended");
          expect(
            await b.evaluate(() =>
              window.__callMediaHarness.connectionWaiters(),
            ),
          ).toBe(0);
          expect(await b.evaluate(() => window.__callIceGate.listeners())).toBe(
            0,
          );
          await b.evaluate(() => window.__callIceGate.release());
          expect(answers.length).toBe(0);
          for (const page of [a, b]) {
            await expect
              .poll(() =>
                page.evaluate(() => window.__callMediaHarness.phase()),
              )
              .toBe("idle");
            await expect
              .poll(() =>
                page.evaluate(() => window.__callMediaHarness.cleaned()),
              )
              .toBe(true);
          }
          return;
        }
        await b.evaluate(() => window.__callIceGate.release());
        await b.clock.resume();
      }
      if ("deferredAnswer" in scenario) {
        await answerObserved;
        // A real native negotiation may remain pending beyond Playwright's
        // default assertion timeout while the production call is still live.
        // Hold precisely that boundary, then use the real SDP and media engine.
        answerDelay = setTimeout(releaseAnswer, 6_000);
      }
      for (const [index, page] of [a, b].entries()) {
        await page.evaluate(() =>
          window.__callMediaHarness.waitForConnection(),
        );
        expect(
          await page.evaluate(() =>
            window.__callMediaHarness.connectionWaiters(),
          ),
        ).toBe(0);
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
      clearTimeout(answerDelay);
      releaseAnswer();
      if ("deferredIce" in scenario)
        await b.evaluate(() => window.__callIceGate?.release()).catch(() => {});
      await Promise.all([contextA.close(), contextB.close()]);
    }
  });
}
