// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { WebRtcTestDialog } from "./WebRtcTestDialog";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function capture() {
  const stop = vi.fn();
  const track = { stop } as unknown as MediaStreamTrack;
  const stream = {
    getTracks: () => [track],
    getVideoTracks: () => [track],
    getAudioTracks: () => [track],
  } as unknown as MediaStream;
  return { stream, stop };
}

let root: Root | undefined;
let host: HTMLDivElement;
const getUserMedia = vi.fn();
const enumerateDevices = vi.fn();
const createPeer = vi.fn();

class TestPeer {
  onicecandidate: ((event: { candidate: { type: string } }) => void) | null =
    null;
  ontrack: ((event: { streams: MediaStream[] }) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  connectionState = "new";
  close = vi.fn();
  addTrack = vi.fn();
  addIceCandidate = vi.fn().mockResolvedValue(undefined);
  createOffer = vi.fn().mockResolvedValue({ type: "offer" });
  createAnswer = vi.fn().mockResolvedValue({ type: "answer" });
  setLocalDescription = vi.fn().mockResolvedValue(undefined);
  setRemoteDescription = vi.fn().mockResolvedValue(undefined);
  constructor() {
    createPeer(this);
  }
}

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("RTCPeerConnection", TestPeer);
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia, enumerateDevices },
  });
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
  getUserMedia.mockReset();
  enumerateDevices.mockReset().mockReturnValue(new Promise(() => {}));
  createPeer.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root?.render(createElement(WebRtcTestDialog)));
  await click('[data-testid="sidebar-action-webrtc-test"]');
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function click(selector: string) {
  const button = document.querySelector<HTMLButtonElement>(selector);
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}

async function close() {
  const button = [...document.querySelectorAll("button")].find(
    (item) => item.textContent === "common.close",
  );
  expect(button).toBeDefined();
  await act(async () => button!.click());
}

for (const boundary of ["close", "unmount"] as const) {
  it(`stops capture that resolves after ${boundary}`, async () => {
    const pending = deferred<MediaStream>();
    const media = capture();
    getUserMedia.mockReturnValue(pending.promise);
    await click('[data-testid="webrtc-test-run"]');
    if (boundary === "close") await close();
    else {
      await act(async () => root?.unmount());
      root = undefined;
    }
    await act(async () => pending.resolve(media.stream));
    expect(media.stop).toHaveBeenCalledOnce();
    expect(enumerateDevices).not.toHaveBeenCalled();
    expect(createPeer).not.toHaveBeenCalled();
  });

  it(`does not create peer connections after ${boundary} during enumeration`, async () => {
    const media = capture();
    const devices = deferred<MediaDeviceInfo[]>();
    getUserMedia.mockResolvedValue(media.stream);
    enumerateDevices.mockReturnValue(devices.promise);
    await click('[data-testid="webrtc-test-run"]');
    expect(enumerateDevices).toHaveBeenCalledOnce();
    if (boundary === "close") await close();
    else {
      await act(async () => root?.unmount());
      root = undefined;
    }
    await act(async () => devices.resolve([]));
    expect(media.stop).toHaveBeenCalledOnce();
    expect(createPeer).not.toHaveBeenCalled();
  });
}

it("allows a new run after reopening and keeps a late capture out of its preview", async () => {
  const first = deferred<MediaStream>();
  const oldMedia = capture();
  const currentMedia = capture();
  getUserMedia
    .mockReturnValueOnce(first.promise)
    .mockResolvedValueOnce(currentMedia.stream);
  await click('[data-testid="webrtc-test-run"]');
  await close();
  await click('[data-testid="sidebar-action-webrtc-test"]');
  await click('[data-testid="webrtc-test-run"]');
  expect(getUserMedia).toHaveBeenCalledTimes(2);
  await act(async () => first.resolve(oldMedia.stream));
  expect(oldMedia.stop).toHaveBeenCalledOnce();
  expect(currentMedia.stop).not.toHaveBeenCalled();
  expect(document.querySelector("video")?.srcObject).toBe(currentMedia.stream);
  expect(enumerateDevices).toHaveBeenCalledOnce();
  await close();
  expect(currentMedia.stop).toHaveBeenCalledOnce();
});

const negotiationStages = [
  [0, "createOffer", 0, "setLocalDescription"],
  [0, "setLocalDescription", 1, "setRemoteDescription"],
  [1, "setRemoteDescription", 1, "createAnswer"],
  [1, "createAnswer", 1, "setLocalDescription"],
  [1, "setLocalDescription", 0, "setRemoteDescription"],
  [0, "setRemoteDescription", null, null],
] as const;

for (const [peerIndex, method, nextIndex, nextMethod] of negotiationStages) {
  it(`closes owned peers and ignores late peer ${peerIndex} ${method} completion`, async () => {
    const pending = deferred<unknown>();
    const media = capture();
    const peers: TestPeer[] = [];
    createPeer.mockImplementation((peer: TestPeer) => {
      if (peers.length === peerIndex)
        peer[method].mockReturnValue(pending.promise);
      peers.push(peer);
    });
    getUserMedia.mockResolvedValue(media.stream);
    enumerateDevices.mockResolvedValue([]);
    await click('[data-testid="webrtc-test-run"]');
    expect(peers[peerIndex][method]).toHaveBeenCalledOnce();
    const oldIce = peers[0].onicecandidate!;
    const oldTrack = peers[1].ontrack!;
    await close();
    expect(peers.every((peer) => peer.close.mock.calls.length === 1)).toBe(
      true,
    );
    expect(media.stop).toHaveBeenCalledOnce();
    await click('[data-testid="sidebar-action-webrtc-test"]');
    getUserMedia.mockReturnValue(new Promise(() => {}));
    await click('[data-testid="webrtc-test-run"]');
    await act(async () => {
      pending.resolve({ type: "offer" });
      oldIce({ candidate: { type: "host" } });
      oldTrack({ streams: [media.stream] });
    });
    if (nextIndex !== null)
      expect(peers[nextIndex][nextMethod]).not.toHaveBeenCalled();
    expect(peers[1].addIceCandidate).not.toHaveBeenCalled();
    expect(document.querySelectorAll("video")[1].srcObject).toBeNull();
    expect(
      document.querySelector<HTMLButtonElement>(
        '[data-testid="webrtc-test-run"]',
      )!.disabled,
    ).toBe(true);
    expect(document.body.textContent).not.toContain("webrtcTest.connectDetail");
  });
}

it("closes both peers and stops capture when unmounted during negotiation", async () => {
  const peers: TestPeer[] = [];
  createPeer.mockImplementation((peer: TestPeer) => peers.push(peer));
  const media = capture();
  getUserMedia.mockResolvedValue(media.stream);
  enumerateDevices.mockResolvedValue([]);
  await click('[data-testid="webrtc-test-run"]');
  expect(peers).toHaveLength(2);
  await act(async () => root?.unmount());
  root = undefined;
  expect(peers.every((peer) => peer.close.mock.calls.length === 1)).toBe(true);
  expect(media.stop).toHaveBeenCalledOnce();
});

it("still completes the loopback and keeps previews alive until close", async () => {
  const media = capture();
  const remote = capture();
  const peers: TestPeer[] = [];
  createPeer.mockImplementation((peer: TestPeer) => peers.push(peer));
  getUserMedia.mockResolvedValue(media.stream);
  enumerateDevices.mockResolvedValue([]);
  await click('[data-testid="webrtc-test-run"]');
  await act(async () => {
    peers[0].connectionState = "connected";
    peers[0].onconnectionstatechange?.();
    peers[1].ontrack?.({ streams: [remote.stream] });
  });
  expect(document.body.textContent).toContain("webrtcTest.connectDetail");
  expect(document.body.textContent).toContain("webrtcTest.mediaDetail");
  expect(document.querySelectorAll("video")[1].srcObject).toBe(remote.stream);
  expect(media.stop).not.toHaveBeenCalled();
  expect(peers.every((peer) => peer.close.mock.calls.length === 0)).toBe(true);
  await close();
  expect(media.stop).toHaveBeenCalledOnce();
  expect(peers.every((peer) => peer.close.mock.calls.length === 1)).toBe(true);
});

it("releases a connected run closed before remote media arrives", async () => {
  const media = capture();
  const nextMedia = capture();
  const peers: TestPeer[] = [];
  createPeer.mockImplementation((peer: TestPeer) => peers.push(peer));
  getUserMedia
    .mockResolvedValueOnce(media.stream)
    .mockResolvedValueOnce(nextMedia.stream);
  enumerateDevices
    .mockResolvedValueOnce([])
    .mockReturnValueOnce(new Promise(() => {}));
  await click('[data-testid="webrtc-test-run"]');
  const lateTrack = peers[1].ontrack!;
  const lateConnectionStateChange = peers[0].onconnectionstatechange!;
  await act(async () => {
    peers[0].connectionState = "connected";
    peers[0].onconnectionstatechange?.();
  });
  expect(document.body.textContent).toContain("webrtcTest.connectDetail");
  expect(document.body.textContent).not.toContain("webrtcTest.mediaDetail");
  await close();
  expect(media.stop).toHaveBeenCalledOnce();
  for (const peer of peers) {
    expect(peer.close).toHaveBeenCalledOnce();
    expect(peer.ontrack).toBeNull();
    expect(peer.onconnectionstatechange).toBeNull();
  }
  await click('[data-testid="sidebar-action-webrtc-test"]');
  await click('[data-testid="webrtc-test-run"]');
  await act(async () => {
    lateTrack({ streams: [media.stream] });
    lateConnectionStateChange();
  });
  expect(document.querySelectorAll("video")[0].srcObject).toBe(
    nextMedia.stream,
  );
  expect(document.querySelectorAll("video")[1].srcObject).toBeNull();
  expect(nextMedia.stop).not.toHaveBeenCalled();
  expect(document.body.textContent).not.toContain("webrtcTest.connectDetail");
  expect(document.body.textContent).not.toContain("webrtcTest.mediaDetail");
  expect(
    document.querySelector<HTMLButtonElement>(
      '[data-testid="webrtc-test-run"]',
    )!.disabled,
  ).toBe(true);
});
