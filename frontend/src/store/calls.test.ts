import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Capture outbound signaling: `calls.signal` → invoke("send_call_signal", {target, payload}).
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useCalls } from "./calls";
import { useSettings } from "./settings";

// --- Minimal WebRTC + media stubs (test env is "node", so no DOM/WebRTC). ---

/** A no-op RTCPeerConnection whose ICE gathering is already "complete" (so the non-trickle
 * wait resolves at once) and whose localDescription is a fixed SDP. */
class FakePc {
  iceGatheringState = "complete";
  connectionState = "new";
  localDescription = { sdp: "LOCAL_SDP" };
  ontrack: ((e: unknown) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  addTrack() {}
  addEventListener() {}
  removeEventListener() {}
  async createOffer() {
    return { type: "offer", sdp: "OFFER" };
  }
  async createAnswer() {
    return { type: "answer", sdp: "ANSWER" };
  }
  setLocalDescription = vi.fn(async () => {});
  setRemoteDescription = vi.fn(async () => {});
  close() {}
}

const fakeTrack = () => ({ stop() {}, enabled: true });
/** A capture stream that only has a video track when video was requested. */
const fakeStream = (video: boolean) => ({
  getTracks: () => (video ? [fakeTrack(), fakeTrack()] : [fakeTrack()]),
  getVideoTracks: () => (video ? [fakeTrack()] : []),
  getAudioTracks: () => [fakeTrack()],
});

const offer = (from: string, callId: string, video = true, name = "Bob") => ({
  from,
  payload: JSON.stringify({ callId, kind: "offer", sdp: "X", name, video }),
});

/** Flush pending microtasks (e.g. a setRemoteDescription().then(...)). */
const flush = () => new Promise((r) => setTimeout(r, 0));

let getUserMedia: ReturnType<typeof vi.fn>;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  vi.stubGlobal(
    "RTCPeerConnection",
    FakePc as unknown as typeof RTCPeerConnection,
  );
  getUserMedia = vi
    .fn()
    .mockImplementation((c: { video?: boolean }) =>
      Promise.resolve(fakeStream(!!c?.video)),
    );
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia } });
  useSettings.setState({ callsEnabled: true }); // feature on for most tests; gate tested below
  useCalls.getState().teardown();
});

afterEach(() => {
  useCalls.getState().teardown();
  vi.unstubAllGlobals();
});

describe("onSignal — incoming offer", () => {
  it("rings: phase=incoming, peer bound to the AUTHENTICATED from (not a payload field)", () => {
    // The payload self-asserts a different sender; the store must use the event `from`.
    useCalls.getState().onSignal({
      from: "bob-device",
      payload: JSON.stringify({
        callId: "c1",
        kind: "offer",
        sdp: "X",
        name: "Bob",
        from: "mallory",
      }),
    });
    const s = useCalls.getState();
    expect(s.phase).toBe("incoming");
    expect(s.peerId).toBe("bob-device");
    expect(s.peerName).toBe("Bob");
    expect(s.callId).toBe("c1");
  });

  it("ignores a malformed payload without throwing", () => {
    expect(() =>
      useCalls.getState().onSignal({ from: "bob", payload: "not json" }),
    ).not.toThrow();
    expect(useCalls.getState().phase).toBe("idle");
  });

  it("does NOT ring when the calls feature is disabled; declines the caller instead", () => {
    useSettings.setState({ callsEnabled: false });
    useCalls.getState().onSignal(offer("bob", "c1"));
    expect(useCalls.getState().phase).toBe("idle"); // never rang
    expect(invoke).toHaveBeenCalledWith("send_call_signal", {
      target: "bob",
      payload: JSON.stringify({ callId: "c1", kind: "bye", reason: "decline" }),
    });
  });
});

describe("onSignal — busy", () => {
  it("replies bye/busy to a second caller and keeps the active call", () => {
    useCalls.getState().onSignal(offer("bob", "c1"));
    expect(useCalls.getState().phase).toBe("incoming");
    invoke.mockClear();

    useCalls.getState().onSignal(offer("carol", "c2"));

    expect(invoke).toHaveBeenCalledWith("send_call_signal", {
      target: "carol",
      payload: JSON.stringify({ callId: "c2", kind: "bye", reason: "busy" }),
    });
    // The original call is untouched.
    expect(useCalls.getState().peerId).toBe("bob");
    expect(useCalls.getState().phase).toBe("incoming");
  });
});

describe("onSignal — bye routing is scoped to the current call", () => {
  it("a bye from the peer ends the ringing call (decline surfaces a reason)", () => {
    useCalls.getState().onSignal(offer("bob", "c1"));
    useCalls.getState().onSignal({
      from: "bob",
      payload: JSON.stringify({ callId: "c1", kind: "bye", reason: "decline" }),
    });
    const s = useCalls.getState();
    expect(s.phase).toBe("idle");
    expect(s.endedReason).toBe("decline");
  });

  it("ignores a bye for a DIFFERENT call id", () => {
    useCalls.getState().onSignal(offer("bob", "c1"));
    useCalls.getState().onSignal({
      from: "bob",
      payload: JSON.stringify({
        callId: "OTHER",
        kind: "bye",
        reason: "hangup",
      }),
    });
    expect(useCalls.getState().phase).toBe("incoming");
  });

  it("ignores a bye from a DIFFERENT peer", () => {
    useCalls.getState().onSignal(offer("bob", "c1"));
    useCalls.getState().onSignal({
      from: "mallory",
      payload: JSON.stringify({ callId: "c1", kind: "bye", reason: "hangup" }),
    });
    expect(useCalls.getState().phase).toBe("incoming");
  });
});

describe("local actions send the right signal", () => {
  it("decline() sends bye/decline to the caller and resets", () => {
    useCalls.getState().onSignal(offer("bob", "c1"));
    invoke.mockClear();
    useCalls.getState().decline();
    expect(invoke).toHaveBeenCalledWith("send_call_signal", {
      target: "bob",
      payload: JSON.stringify({ callId: "c1", kind: "bye", reason: "decline" }),
    });
    expect(useCalls.getState().phase).toBe("idle");
  });
});

describe("outgoing call (engine happy path)", () => {
  it("startCall captures media, sends an offer, and goes outgoing", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    const s = useCalls.getState();
    expect(s.phase).toBe("outgoing");
    expect(s.peerId).toBe("bob");
    const call = invoke.mock.calls.find((c) => c[0] === "send_call_signal");
    expect(call).toBeTruthy();
    const arg = call![1] as { target: string; payload: string };
    expect(arg.target).toBe("bob");
    const msg = JSON.parse(arg.payload);
    expect(msg.kind).toBe("offer");
    expect(msg.sdp).toBe("LOCAL_SDP");
  });

  it("ends the call as failed if the offer can't be delivered to any device", async () => {
    invoke.mockRejectedValue(new Error("unknown peer")); // send_call_signal rejects
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    // No swallowed failure: the caller is told at once rather than waiting out the timeout.
    expect(useCalls.getState().phase).toBe("idle");
    expect(useCalls.getState().endedReason).toBe("failed");
  });

  it("an answer FROM THE PEER for THIS call advances to connecting", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    const callId = useCalls.getState().callId!;
    useCalls.getState().onSignal({
      from: "bob",
      payload: JSON.stringify({ callId, kind: "answer", sdp: "ANSWER" }),
    });
    await flush();
    expect(useCalls.getState().phase).toBe("connecting");
  });

  it("an answer from a DIFFERENT peer is ignored (no media hijack)", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    const callId = useCalls.getState().callId!;
    useCalls.getState().onSignal({
      from: "mallory",
      payload: JSON.stringify({ callId, kind: "answer", sdp: "EVIL" }),
    });
    await flush();
    expect(useCalls.getState().phase).toBe("outgoing");
  });

  it("if the call is torn down during getUserMedia, the camera is released and no offer is sent", async () => {
    let stopped = 0;
    const trk = () => ({ stop: () => stopped++, enabled: true });
    const t1 = trk();
    const t2 = trk();
    const heldStream = {
      getTracks: () => [t1, t2],
      getVideoTracks: () => [t1],
      getAudioTracks: () => [t2],
    };
    let release!: (s: unknown) => void;
    getUserMedia.mockImplementationOnce(
      () => new Promise((r) => (release = r)),
    );
    const pending = useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    useCalls.getState().hangup(); // user bails while the camera is opening
    release(heldStream); // getUserMedia now resolves
    await pending;
    expect(useCalls.getState().phase).toBe("idle");
    expect(stopped).toBe(2); // both tracks stopped — camera/mic released, not leaked
    const offerSent = invoke.mock.calls.some((c) => {
      try {
        return (
          JSON.parse((c[1] as { payload: string }).payload).kind === "offer"
        );
      } catch {
        return false;
      }
    });
    expect(offerSent).toBe(false); // never dialed the abandoned peer
  });

  it("startCall is a no-op while a call is already active", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    const callId = useCalls.getState().callId;
    await useCalls
      .getState()
      .startCall(
        { name: "Carol", accountId: "carol-acct", deviceIds: ["carol"] },
        true,
      );
    expect(useCalls.getState().peerId).toBe("bob");
    expect(useCalls.getState().callId).toBe(callId);
  });
});

describe("voice vs video are distinct call kinds", () => {
  it("a voice call captures audio-only and advertises video:false in the offer", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        false,
      );
    expect(getUserMedia).toHaveBeenCalledWith({ audio: true, video: false });
    expect(useCalls.getState().hasVideo).toBe(false);
    const call = invoke.mock.calls.find((c) => c[0] === "send_call_signal")!;
    const msg = JSON.parse((call[1] as { payload: string }).payload);
    expect(msg.video).toBe(false);
  });

  it("a video call captures video and advertises video:true", async () => {
    await useCalls
      .getState()
      .startCall(
        { name: "Bob", accountId: "bob-acct", deviceIds: ["bob"] },
        true,
      );
    expect(getUserMedia).toHaveBeenCalledWith({ audio: true, video: true });
    expect(useCalls.getState().hasVideo).toBe(true);
  });

  it("the callee MIRRORS the caller's kind (a voice offer is accepted audio-only)", async () => {
    useCalls.getState().onSignal(offer("bob", "c1", false)); // voice offer
    await useCalls.getState().accept();
    await flush();
    expect(getUserMedia).toHaveBeenCalledWith({ audio: true, video: false });
    expect(useCalls.getState().hasVideo).toBe(false);
  });
});

describe("multi-device fan-out", () => {
  const target = {
    name: "Bob",
    accountId: "bob-acct",
    deviceIds: ["d1", "d2"],
  };

  it("rings EVERY device of the account", async () => {
    await useCalls.getState().startCall(target, true);
    const rung = invoke.mock.calls
      .filter((c) => c[0] === "send_call_signal")
      .map((c) => (c[1] as { target: string }).target);
    expect(rung).toContain("d1");
    expect(rung).toContain("d2");
    expect(useCalls.getState().phase).toBe("outgoing");
  });

  it("first device to answer wins and the others are cancelled", async () => {
    await useCalls.getState().startCall(target, true);
    const callId = useCalls.getState().callId!;
    invoke.mockClear();
    useCalls.getState().onSignal({
      from: "d2",
      payload: JSON.stringify({ callId, kind: "answer", sdp: "ANSWER" }),
    });
    await flush();
    expect(useCalls.getState().phase).toBe("connecting");
    expect(useCalls.getState().peerId).toBe("d2"); // locked to the answerer
    expect(invoke).toHaveBeenCalledWith("send_call_signal", {
      target: "d1", // the losing device is told to stop ringing
      payload: JSON.stringify({ callId, kind: "bye", reason: "hangup" }),
    });
  });

  it("a decline from one device keeps ringing the rest; ends only when all are gone", async () => {
    await useCalls.getState().startCall(target, true);
    const callId = useCalls.getState().callId!;
    useCalls.getState().onSignal({
      from: "d1",
      payload: JSON.stringify({ callId, kind: "bye", reason: "decline" }),
    });
    expect(useCalls.getState().phase).toBe("outgoing"); // d2 still ringing
    useCalls.getState().onSignal({
      from: "d2",
      payload: JSON.stringify({ callId, kind: "bye", reason: "busy" }),
    });
    expect(useCalls.getState().phase).toBe("idle"); // all devices gone
    expect(useCalls.getState().endedReason).toBe("busy");
  });

  it("a late second answer (after one already won) is ignored", async () => {
    await useCalls.getState().startCall(target, true);
    const callId = useCalls.getState().callId!;
    useCalls.getState().onSignal({
      from: "d1",
      payload: JSON.stringify({ callId, kind: "answer", sdp: "A1" }),
    });
    await flush();
    expect(useCalls.getState().peerId).toBe("d1");
    useCalls.getState().onSignal({
      from: "d2",
      payload: JSON.stringify({ callId, kind: "answer", sdp: "A2" }),
    });
    await flush();
    expect(useCalls.getState().peerId).toBe("d1"); // unchanged; the late answer is ignored
  });
});
