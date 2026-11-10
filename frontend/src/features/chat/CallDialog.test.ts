// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { CallButtons, CallDialog } from "./CallDialog";
import { useCalls } from "@/store/calls";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";
import { useSettings } from "@/store/settings";
import { useAuth } from "@/store/auth";
import type { PeerInfo } from "@/lib/types";

vi.mock("react-i18next", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-i18next")>()),
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function remoteStream(video: boolean) {
  const audioTrack = { kind: "audio" } as MediaStreamTrack;
  const videoTrack = { kind: "video" } as MediaStreamTrack;
  return {
    getTracks: () => (video ? [audioTrack, videoTrack] : [audioTrack]),
    getVideoTracks: () => (video ? [videoTrack] : []),
    getAudioTracks: () => [audioTrack],
  } as unknown as MediaStream;
}

let root: Root;
let host: HTMLDivElement;
const originalStartCall = useCalls.getState().startCall;

function peer(id: string, directlyReachable?: boolean): PeerInfo {
  return {
    user_id: id,
    account_id: "bob",
    name: "Bob",
    addr: "192.168.1.2:1234",
    post_office: false,
    discovered_on_lan: false,
    directly_reachable: directlyReachable,
  };
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue();
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 1,
  });
  useSettings.setState({ callsEnabled: true });
  useChat.setState({ peers: [] });
  usePresence.setState({ map: { bob: { online: true, last_seen_secs: 0 } } });
  useCalls.setState({
    phase: "connected",
    callId: "playback-test",
    peerId: null,
    peerName: "Bob",
    localStream: null,
    remoteStream: null,
    endedReason: null,
    error: null,
    failureKind: null,
    retryTarget: null,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  useCalls.getState().teardown();
  useCalls.setState({ startCall: originalStartCall });
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("explains relay-only connectivity before starting capture or signaling", async () => {
  const startCall = vi.fn();
  useCalls.setState({ phase: "idle", startCall });
  useChat.setState({ peers: [peer("relay-device", false)] });
  await act(async () =>
    root.render(createElement(CallButtons, { accountId: "bob", name: "Bob" })),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="conversation-call-voice"]',
      )!
      .click(),
  );
  expect(startCall).not.toHaveBeenCalled();
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "call.unavailableTitle",
  );
});

for (const direct of [true, undefined]) {
  it(`allows ${direct ? "routed direct" : "legacy unknown"} candidates without assuming same-LAN is required`, async () => {
    const startCall = vi.fn();
    useCalls.setState({ phase: "idle", startCall });
    useChat.setState({
      peers: [peer("candidate", direct), peer("relay-device", false)],
    });
    await act(async () =>
      root.render(
        createElement(CallButtons, { accountId: "bob", name: "Bob" }),
      ),
    );
    await act(async () =>
      host
        .querySelector<HTMLButtonElement>(
          '[data-testid="conversation-call-video"]',
        )!
        .click(),
    );
    expect(startCall).toHaveBeenCalledWith(
      { name: "Bob", accountId: "bob", deviceIds: ["candidate"] },
      true,
    );
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });
}

it("rechecks the latest routes when clicked", async () => {
  const startCall = vi.fn();
  useCalls.setState({ phase: "idle", startCall });
  useChat.setState({ peers: [peer("candidate", true)] });
  await act(async () =>
    root.render(createElement(CallButtons, { accountId: "bob", name: "Bob" })),
  );
  await act(async () =>
    useChat.setState({ peers: [peer("candidate", false)] }),
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="conversation-call-voice"]',
      )!
      .click(),
  );
  expect(startCall).not.toHaveBeenCalled();
  expect(document.querySelector('[role="dialog"]')).not.toBeNull();
});

it("keeps a failed call explanation until it is dismissed", async () => {
  vi.useFakeTimers();
  useCalls.setState({ phase: "idle", endedReason: "failed" });
  await act(async () => root.render(createElement(CallDialog)));
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "call.connectionHint",
  );
  expect(
    document.querySelector('[data-testid="call-check-connection"]'),
  ).not.toBeNull();
});

it("retries the original call kind using fresh routes", async () => {
  const startCall = vi.fn();
  useCalls.setState({
    phase: "idle",
    endedReason: "failed",
    startCall,
    retryTarget: {
      owner: "alice",
      generation: 1,
      video: false,
      target: { accountId: "bob", name: "Bob", deviceIds: ["old-device"] },
    },
  });
  useChat.setState({
    peers: [peer("new-device", true), peer("relay-device", false)],
  });
  await act(async () => root.render(createElement(CallDialog)));
  await act(async () =>
    document
      .querySelector<HTMLButtonElement>('[data-testid="call-retry"]')!
      .click(),
  );
  expect(startCall).toHaveBeenCalledWith(
    { accountId: "bob", name: "Bob", deviceIds: ["new-device"] },
    false,
  );
});

it("does not retry through a route that has disappeared", async () => {
  const startCall = vi.fn();
  useCalls.setState({
    phase: "idle",
    endedReason: "failed",
    startCall,
    retryTarget: {
      owner: "alice",
      generation: 1,
      video: true,
      target: { accountId: "bob", name: "Bob", deviceIds: ["old-device"] },
    },
  });
  useChat.setState({ peers: [peer("old-device", false)] });
  await act(async () => root.render(createElement(CallDialog)));
  await act(async () =>
    document
      .querySelector<HTMLButtonElement>('[data-testid="call-retry"]')!
      .click(),
  );
  expect(startCall).not.toHaveBeenCalled();
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain(
    "call.unavailableTitle",
  );
});

it("removes retry when the authentication generation changes", async () => {
  useCalls.setState({
    phase: "idle",
    endedReason: "failed",
    retryTarget: {
      owner: "alice",
      generation: 1,
      video: false,
      target: { accountId: "bob", name: "Bob", deviceIds: ["old-device"] },
    },
  });
  await act(async () => root.render(createElement(CallDialog)));
  expect(document.querySelector('[data-testid="call-retry"]')).not.toBeNull();
  await act(async () => useAuth.setState({ generation: 2 }));
  expect(document.querySelector('[data-testid="call-retry"]')).toBeNull();
});

for (const video of [false, true]) {
  it(`plays remote ${video ? "video" : "audio-only"} without muting it`, async () => {
    const stream = remoteStream(video);
    useCalls.setState({ video, remoteStream: stream });
    await act(async () => root.render(createElement(CallDialog)));
    const player = [
      ...host.querySelectorAll<HTMLMediaElement>("audio,video"),
    ].find((element) => element.srcObject === stream);
    expect(player).toBeDefined();
    expect(player!.muted).toBe(false);
    expect(player!.play).toHaveBeenCalled();
  });
}

it("pauses and detaches remote playback when the dialog unmounts", async () => {
  const stream = remoteStream(true);
  useCalls.setState({ video: true, remoteStream: stream });
  await act(async () => root.render(createElement(CallDialog)));
  const player = host.querySelector("video")!;
  expect(player.srcObject).toBe(stream);
  await act(async () => root.render(null));
  expect(player.pause).toHaveBeenCalled();
  expect(player.srcObject).toBeNull();
});

it("preserves playback when a video stream is replaced by audio-only fallback", async () => {
  const video = remoteStream(true);
  const audio = remoteStream(false);
  useCalls.setState({ video: true, remoteStream: video });
  await act(async () => root.render(createElement(CallDialog)));
  const previousPlayer = host.querySelector("video")!;
  await act(async () => useCalls.setState({ remoteStream: audio }));
  const player = [
    ...host.querySelectorAll<HTMLMediaElement>("audio,video"),
  ].find((element) => element.srcObject === audio);
  expect(player).toBeDefined();
  expect(previousPlayer.srcObject).not.toBe(video);
});
