// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { openMediaDeviceCheck } from "./mediaDeviceCheck";
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
function media(kind = "audio") {
  const stop = vi.fn();
  const stream = {
    getTracks: () => [{ stop, kind }],
  } as unknown as MediaStream;
  return { stream, stop };
}
it("stops a late capture after cancellation", async () => {
  let resolve!: (stream: MediaStream) => void;
  const getUserMedia = vi.fn(
    () =>
      new Promise<MediaStream>((r) => {
        resolve = r;
      }),
  );
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia, enumerateDevices: vi.fn() },
  });
  const controller = new AbortController();
  const pending = openMediaDeviceCheck("audio", controller.signal);
  controller.abort();
  await expect(pending).rejects.toMatchObject({ code: "cancelled" });
  const capture = media();
  resolve(capture.stream);
  await Promise.resolve();
  expect(capture.stop).toHaveBeenCalledOnce();
  expect(getUserMedia).toHaveBeenCalledWith({ audio: true, video: false });
});
it("bounds enumeration and releases acquired tracks on timeout", async () => {
  vi.useFakeTimers();
  const capture = media("video");
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: vi.fn(async () => capture.stream),
      enumerateDevices: () => new Promise(() => {}),
    },
  });
  const pending = openMediaDeviceCheck("video", new AbortController().signal, {
    timeoutMs: 20,
  });
  const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });
  await vi.advanceTimersByTimeAsync(25);
  await rejected;
  expect(capture.stop).toHaveBeenCalledOnce();
});
it("owns a successful device session until explicitly stopped", async () => {
  const capture = media("video");
  const getUserMedia = vi.fn(async () => capture.stream);
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia,
      enumerateDevices: async () => [{ kind: "videoinput" }],
    },
  });
  const session = await openMediaDeviceCheck(
    "video",
    new AbortController().signal,
  );
  expect(session.devices).toBe(1);
  expect(capture.stop).not.toHaveBeenCalled();
  session.stop();
  session.stop();
  expect(capture.stop).toHaveBeenCalledOnce();
  expect(getUserMedia).toHaveBeenCalledWith({ audio: false, video: true });
});

class Peer {
  onicecandidate: unknown;
  close = vi.fn();
  addTrack = vi.fn();
  addIceCandidate = vi.fn(async () => {});
  createOffer = vi.fn(async () => ({ type: "offer" }));
  createAnswer = vi.fn(async () => ({ type: "answer" }));
  setLocalDescription = vi.fn(async () => {});
  setRemoteDescription = vi.fn(async () => {});
  getStats = vi.fn(async () => new Map());
}
async function fixture() {
  const capture = media();
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: async () => capture.stream,
      enumerateDevices: async () => [],
    },
  });
  const controller = new AbortController();
  const session = await openMediaDeviceCheck("audio", controller.signal, {
    timeoutMs: 500,
  });
  return { capture, controller, session };
}
for (const [index, method] of [
  [0, "createOffer"],
  [0, "setLocalDescription"],
  [1, "setRemoteDescription"],
  [1, "createAnswer"],
  [1, "setLocalDescription"],
  [0, "setRemoteDescription"],
] as const) {
  it(`retires pending peer ${index} ${method} and prevents later negotiation`, async () => {
    const { capture, controller, session } = await fixture();
    const peers: Peer[] = [];
    let resolve!: () => void;
    vi.stubGlobal(
      "RTCPeerConnection",
      class extends Peer {
        constructor() {
          super();
          peers.push(this);
          if (peers.length - 1 === index)
            this[method] = vi.fn(
              () =>
                new Promise<void>((r) => {
                  resolve = r;
                }),
            ) as never;
        }
      },
    );
    const pending = session.runLoopback();
    const rejected = expect(pending).rejects.toMatchObject({
      code: "cancelled",
    });
    for (let i = 0; i < 12; i++) await Promise.resolve();
    controller.abort();
    await rejected;
    resolve();
    await Promise.resolve();
    expect(peers).toHaveLength(2);
    expect(peers.every((peer) => peer.close.mock.calls.length === 1)).toBe(
      true,
    );
    expect(capture.stop).toHaveBeenCalledOnce();
  });
}
it("requires advancing decoded audio and bytes, then releases loopback peers", async () => {
  vi.useFakeTimers();
  const { session, capture } = await fixture();
  const peers: Peer[] = [];
  let sample = 0;
  vi.stubGlobal(
    "RTCPeerConnection",
    class extends Peer {
      constructor() {
        super();
        peers.push(this);
        this.getStats = vi.fn(
          async () =>
            new Map([
              [
                "audio",
                {
                  type: "inbound-rtp",
                  kind: "audio",
                  bytesReceived: ++sample * 100,
                  totalSamplesReceived: sample * 10,
                },
              ],
            ]),
        );
      }
    },
  );
  const pending = session.runLoopback();
  await vi.advanceTimersByTimeAsync(150);
  expect(await pending).toMatchObject({
    bytesReceived: 200,
    totalSamplesReceived: 20,
  });
  expect(peers.every((peer) => peer.close.mock.calls.length === 1)).toBe(true);
  expect(capture.stop).not.toHaveBeenCalled();
  session.stop();
  expect(capture.stop).toHaveBeenCalledOnce();
});
it("cannot report success for signaling without decoded samples", async () => {
  vi.useFakeTimers();
  const { session, capture } = await fixture();
  vi.stubGlobal(
    "RTCPeerConnection",
    class extends Peer {
      constructor() {
        super();
        this.getStats = vi.fn(
          async () =>
            new Map([
              [
                "audio",
                { type: "inbound-rtp", kind: "audio", bytesReceived: 100 },
              ],
            ]),
        );
      }
    },
  );
  const pending = session.runLoopback();
  const rejected = expect(pending).rejects.toMatchObject({
    code: "unsupported",
  });
  await vi.advanceTimersByTimeAsync(600);
  await rejected;
  expect(capture.stop).toHaveBeenCalledOnce();
});
it("bounds a getStats promise that never returns", async () => {
  vi.useFakeTimers();
  const { session, capture } = await fixture();
  vi.stubGlobal(
    "RTCPeerConnection",
    class extends Peer {
      constructor() {
        super();
        this.getStats = vi.fn(() => new Promise(() => {}));
      }
    },
  );
  const pending = session.runLoopback();
  const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });
  await vi.advanceTimersByTimeAsync(600);
  await rejected;
  expect(capture.stop).toHaveBeenCalledOnce();
});

it("a denied camera does not prevent a separate microphone check", async () => {
  const capture = media();
  const getUserMedia = vi
    .fn()
    .mockRejectedValueOnce(new DOMException("Denied", "NotAllowedError"))
    .mockResolvedValueOnce(capture.stream);
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia,
      enumerateDevices: async () => [{ kind: "audioinput" }],
    },
  });
  await expect(
    openMediaDeviceCheck("video", new AbortController().signal),
  ).rejects.toMatchObject({ code: "permission" });
  const session = await openMediaDeviceCheck(
    "audio",
    new AbortController().signal,
  );
  expect(session.devices).toBe(1);
  session.stop();
  expect(capture.stop).toHaveBeenCalledOnce();
});
it("releases capture that arrives after its deadline", async () => {
  vi.useFakeTimers();
  let resolve!: (stream: MediaStream) => void;
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: () =>
        new Promise<MediaStream>((done) => {
          resolve = done;
        }),
      enumerateDevices: vi.fn(),
    },
  });
  const pending = openMediaDeviceCheck("audio", new AbortController().signal, {
    timeoutMs: 10,
  });
  const rejected = expect(pending).rejects.toMatchObject({ code: "timeout" });
  await vi.advanceTimersByTimeAsync(20);
  await rejected;
  const capture = media();
  resolve(capture.stream);
  await Promise.resolve();
  expect(capture.stop).toHaveBeenCalledOnce();
});
