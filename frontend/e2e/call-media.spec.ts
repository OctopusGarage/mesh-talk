import { test, expect, type Page } from "@playwright/test";

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
type Harness = {
  phase: () => string;
  signal: (from: string, payload: string) => void;
  start: (target: string) => Promise<void>;
  accept: () => Promise<void>;
  hangup: () => void;
  stats: () => Promise<Record<string, number | boolean>>;
  cleaned: () => boolean;
};
declare global {
  interface Window {
    __callMediaHarness: Harness;
  }
}

async function prepare(page: Page, url: string, id: string) {
  await page.route("**/__call-media", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><title>Call media verification</title><script type="module">
      import { useCalls } from '/src/store/calls.ts';
      import { useAuth } from '/src/store/auth.ts';
      import { useSettings } from '/src/store/settings.ts';
      const connections = [], tracks = [];
      const NativeConnection = window.RTCPeerConnection;
      window.RTCPeerConnection = class extends NativeConnection {
        constructor(options) { super(options); connections.push(this); }
      };
      window.__TAURI_INTERNALS__ = { invoke: async (command, args) => {
        if (command === 'send_call_signal') return window.__deliverCallSignal(args);
        if (command === 'initiate_privacy_contact') return null;
        throw new Error('Unexpected IPC: ' + command);
      }};
      useAuth.setState({ user: { id: ${JSON.stringify(id)}, display_name: ${JSON.stringify(id)} } });
      useSettings.setState({ callsEnabled: true });
      useCalls.subscribe(state => {
        for (const track of state.localStream?.getTracks() ?? []) {
          if (!tracks.includes(track)) tracks.push(track);
        }
      });
      window.__callMediaHarness = {
        phase: () => useCalls.getState().phase,
        signal: (from, payload) => useCalls.getState().onSignal({ from, payload }),
        start: target => useCalls.getState().startCall({ name: target, accountId: target, deviceIds: [target] }, true),
        accept: () => useCalls.getState().accept(),
        hangup: () => useCalls.getState().hangup(),
        stats: async () => {
          const result = { audioSent: 0, audioReceived: 0, videoSent: 0, videoReceived: 0, dtls: false };
          for (const entry of (await connections[0].getStats()).values()) {
            if (entry.type === 'transport' && entry.dtlsState === 'connected') result.dtls = true;
            if (entry.kind === 'audio' || entry.kind === 'video') {
              if (entry.type === 'outbound-rtp') result[entry.kind + 'Sent'] += entry.bytesSent ?? 0;
              if (entry.type === 'inbound-rtp') result[entry.kind + 'Received'] += entry.bytesReceived ?? 0;
            }
          }
          return result;
        },
        cleaned: () => tracks.length === 2 && tracks.every(track => track.readyState === 'ended') &&
          connections.every(connection => connection.connectionState === 'closed') &&
          useCalls.getState().localStream === null && useCalls.getState().remoteStream === null,
      };
    </script>`,
    }),
  );
  await page.goto(`${url}/__call-media`);
  await expect
    .poll(() => page.evaluate(() => Boolean(window.__callMediaHarness)))
    .toBe(true);
}

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
      prepare(a, baseURL!, "device-a"),
      prepare(b, baseURL!, "device-b"),
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
