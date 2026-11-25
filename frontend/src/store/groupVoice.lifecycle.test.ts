import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  joinGroupVoice,
  startGroupVoice,
  stopGroupVoice,
  muteGroupVoice,
  handleGroupVoiceSignal,
  parseGroupVoice,
  useGroupVoice,
} from "./groupVoice";
import { useSettings } from "./settings";
import { notifySyncInvalidation } from "@/lib/syncInvalidation";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const sdp =
  "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=candidate:local 1 udp 1 127.0.0.1 4000 typ host";
class Peer extends EventTarget {
  static all: Peer[] = [];
  signalingState = "stable";
  connectionState = "new";
  iceGatheringState = "complete";
  localDescription: { type: string; sdp: string } | null = null;
  ontrack: ((event: { track: { kind: string } }) => void) | null = null;
  onconnectionstatechange: (() => void) | null = null;
  addTrack = vi.fn();
  close = vi.fn();
  createOffer = vi.fn(async () => ({ type: "offer", sdp }));
  createAnswer = vi.fn(async () => ({ type: "answer", sdp }));
  setLocalDescription = vi.fn(
    async (description: { type: string; sdp: string }) => {
      this.localDescription = description;
      this.signalingState =
        description.type === "offer" ? "have-local-offer" : "stable";
    },
  );
  setRemoteDescription = vi.fn(async (_description: unknown) => {});
  constructor() {
    super();
    Peer.all.push(this);
  }
}
const invite = () => ({
  group: "g",
  meeting: "meeting",
  host: "host",
  expires: Date.now() + 120000,
});
const signal = (from: string, kind: string, extra = {}, meeting = "meeting") =>
  handleGroupVoiceSignal(
    {
      from,
      payload: JSON.stringify({
        protocol: "group-voice-1",
        group: "g",
        meeting,
        kind,
        ...extra,
      }),
    },
    "local",
  );
const fields = (args: Parameters<typeof invoke>[1]): Record<string, unknown> =>
  args &&
  !Array.isArray(args) &&
  !(args instanceof ArrayBuffer) &&
  !ArrayBuffer.isView(args)
    ? args
    : {};
const sent = () =>
  vi.mocked(invoke).mock.calls.flatMap(([command, args]) => {
    const { payload, target } = fields(args);
    return command === "send_call_signal" && typeof payload === "string"
      ? [{ to: target, packet: parseGroupVoice(payload) }]
      : [];
  });
const flush = async () => {
  for (let turn = 0; turn < 30; turn++) await Promise.resolve();
};
let track: { kind: string; enabled: boolean; stop: ReturnType<typeof vi.fn> };
let membership: Map<string, string | null>;
beforeEach(() => {
  vi.useFakeTimers();
  Peer.all = [];
  membership = new Map();
  track = { kind: "audio", enabled: true, stop: vi.fn() };
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: vi.fn().mockResolvedValue({
        getTracks: () => [track],
        getAudioTracks: () => [track],
      }),
    },
  });
  vi.stubGlobal("RTCPeerConnection", Peer);
  vi.stubGlobal(
    "MediaStream",
    class {
      constructor(public tracks: unknown[]) {}
    },
  );
  vi.mocked(invoke).mockImplementation(async (command, args) =>
    command === "group_voice_member"
      ? membership.has(String(fields(args).deviceId))
        ? membership.get(String(fields(args).deviceId))
        : `account:${fields(args).deviceId}`
      : undefined,
  );
  useSettings.setState({ callsEnabled: true });
});
afterEach(() => {
  stopGroupVoice();
  useSettings.setState({ callsEnabled: false });
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});
async function joined() {
  await joinGroupVoice(invite(), "local", "owner");
  signal("host", "roster", { devices: ["host", "local"], revision: 1 });
  await flush();
  expect(useGroupVoice.getState().active).toEqual({
    group: "g",
    host: false,
    joining: false,
  });
}
describe("group voice lifecycle", () => {
  it("expires invites without capture and retires unanswered joins", async () => {
    await joinGroupVoice(
      { ...invite(), expires: Date.now() },
      "local",
      "owner",
    );
    expect(navigator.mediaDevices.getUserMedia).not.toHaveBeenCalled();
    expect(useGroupVoice.getState().error).toBe("groupVoice.expired");
    await joinGroupVoice(invite(), "local", "owner");
    expect(sent().some((item) => item.packet?.kind === "join")).toBe(true);
    await vi.advanceTimersByTimeAsync(20000);
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.denied",
    });
    expect(track.stop).toHaveBeenCalledOnce();
  });
  it("matches answers to connections and cleans remote audio on stop", async () => {
    await joined();
    const pc = Peer.all[0];
    const offer = sent().find((item) => item.packet?.kind === "offer")?.packet;
    if (offer?.kind !== "offer") throw new Error("missing offer");
    expect(pc.addTrack).toHaveBeenCalledWith(track, expect.anything());
    signal("host", "answer", { sdp, revision: 1, connection: "stale" });
    await flush();
    expect(pc.setRemoteDescription).not.toHaveBeenCalled();
    signal("host", "answer", {
      sdp,
      revision: 1,
      connection: offer.connection,
    });
    await flush();
    expect(pc.setRemoteDescription).toHaveBeenCalledWith({
      type: "answer",
      sdp,
    });
    pc.ontrack?.({ track: { kind: "video" } });
    expect(useGroupVoice.getState().audio).toEqual({});
    pc.ontrack?.({ track: { kind: "audio" } });
    expect(useGroupVoice.getState().audio.host).toBeDefined();
    muteGroupVoice();
    expect(track.enabled).toBe(false);
    muteGroupVoice();
    expect(track.enabled).toBe(true);
    const late = pc.ontrack;
    stopGroupVoice(true);
    late?.({ track: { kind: "audio" } });
    expect(useGroupVoice.getState().audio).toEqual({});
    expect(pc.close).toHaveBeenCalledOnce();
    expect(pc.ontrack).toBeNull();
    expect(pc.onconnectionstatechange).toBeNull();
    expect(track.stop).toHaveBeenCalledOnce();
    expect(
      sent().some(
        (item) => item.to === "host" && item.packet?.kind === "leave",
      ),
    ).toBe(true);
  });
  it("clears establishment deadlines and bounds disconnected recovery", async () => {
    await joined();
    const pc = Peer.all[0];
    pc.connectionState = "connected";
    pc.onconnectionstatechange?.();
    await vi.advanceTimersByTimeAsync(25000);
    expect(useGroupVoice.getState().active).not.toBeNull();
    pc.connectionState = "disconnected";
    pc.onconnectionstatechange?.();
    await vi.advanceTimersByTimeAsync(19999);
    expect(pc.close).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(pc.close).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.failed",
    });
  });
  it("rejects duplicate-account rosters and permits only the current host to end", async () => {
    await joinGroupVoice(invite(), "local", "owner");
    membership.set("outsider", "account:host");
    signal("host", "roster", {
      devices: ["host", "local", "outsider"],
      revision: 1,
    });
    await flush();
    expect(Peer.all).toHaveLength(0);
    expect(useGroupVoice.getState().active?.joining).toBe(true);
    signal("host", "roster", { devices: ["host", "local"], revision: 2 });
    await flush();
    signal("outsider", "end");
    await flush();
    expect(useGroupVoice.getState().active).not.toBeNull();
    signal("host", "end");
    await flush();
    expect(useGroupVoice.getState().active).toBeNull();
    expect(track.stop).toHaveBeenCalledOnce();
  });
  it("rechecks access only for this owner's document invalidations", async () => {
    await joined();
    membership.set("local", null);
    notifySyncInvalidation({ owner: "other", kind: "state" });
    notifySyncInvalidation({
      owner: "owner",
      kind: "state",
      scope: "read_receipts",
    });
    await flush();
    expect(track.stop).not.toHaveBeenCalled();
    notifySyncInvalidation({
      owner: "owner",
      kind: "state",
      scope: "documents",
    });
    await flush();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.access",
    });
    expect(track.stop).toHaveBeenCalledOnce();
  });
  it("answers only an expected offer and closes a departing member's leg", async () => {
    await startGroupVoice("g", "local", "owner", ["host"]);
    const meeting = sent()[0]?.packet?.meeting;
    expect(meeting).toBeDefined();
    signal("host", "join", {}, meeting);
    await flush();
    signal(
      "host",
      "offer",
      { sdp, revision: 99, connection: "wrong" },
      meeting,
    );
    await flush();
    expect(Peer.all).toHaveLength(0);
    signal(
      "host",
      "offer",
      { sdp, revision: 1, connection: "expected" },
      meeting,
    );
    await flush();
    expect(Peer.all).toHaveLength(1);
    expect(Peer.all[0].setRemoteDescription).toHaveBeenCalledWith({
      type: "offer",
      sdp,
    });
    expect(
      sent().some(
        (item) =>
          item.packet?.kind === "answer" &&
          item.packet.connection === "expected",
      ),
    ).toBe(true);
    signal("host", "leave", {}, meeting);
    await flush();
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState().active?.host).toBe(true);
  });
  it("rolls back an unacknowledged admission without admitting the new device", async () => {
    await startGroupVoice("g", "local", "owner", ["a", "b"]);
    const meeting = sent()[0]?.packet?.meeting;
    signal("a", "join", {}, meeting);
    await flush();
    expect(
      sent().some(
        (item) =>
          item.to === "a" &&
          item.packet?.kind === "roster" &&
          item.packet.revision === 1,
      ),
    ).toBe(true);
    signal("b", "join", {}, meeting);
    await flush();
    expect(
      sent().some(
        (item) =>
          item.to === "a" &&
          item.packet?.kind === "roster" &&
          item.packet.revision === 2,
      ),
    ).toBe(true);
    await vi.advanceTimersByTimeAsync(10000);
    await flush();
    expect(
      sent().some((item) => item.to === "b" && item.packet?.kind === "roster"),
    ).toBe(false);
    expect(sent()).toContainEqual({
      to: "a",
      packet: {
        group: "g",
        meeting,
        kind: "roster",
        revision: 3,
        devices: ["local", "a"],
      },
    });
    stopGroupVoice(true, true);
    expect(
      sent().some((item) => item.to === "a" && item.packet?.kind === "end"),
    ).toBe(true);
    expect(track.stop).toHaveBeenCalledOnce();
  });

  it("cancels a pending join on leave and ignores its late acknowledgement", async () => {
    await startGroupVoice("g", "local", "owner", ["a", "b"]);
    const meeting = sent()[0]?.packet?.meeting;
    signal("a", "join", {}, meeting);
    await flush();
    signal("b", "join", {}, meeting);
    await flush();
    signal("b", "leave", {}, meeting);
    await flush();
    signal("a", "ready", { revision: 2 }, meeting);
    await flush();
    expect(
      sent().some((item) => item.to === "b" && item.packet?.kind === "roster"),
    ).toBe(false);
    expect(sent()).toContainEqual({
      to: "a",
      packet: {
        group: "g",
        meeting,
        kind: "roster",
        revision: 3,
        devices: ["local", "a"],
      },
    });
  });

  it("cleans microphone ownership when the join transport rejects", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "group_voice_member") return "account";
      throw new Error("offline");
    });
    await joinGroupVoice(invite(), "local", "owner");
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.unreachable",
    });
    expect(track.stop).toHaveBeenCalledOnce();
  });

  it("removes failed answer legs and rejects callbacks retained after cancellation", async () => {
    await joined();
    const pc = Peer.all[0];
    const offer = sent().find((item) => item.packet?.kind === "offer")?.packet;
    if (offer?.kind !== "offer") throw new Error("missing offer");
    const lateState = pc.onconnectionstatechange;
    pc.setRemoteDescription.mockRejectedValueOnce(new Error("invalid answer"));
    signal("host", "answer", {
      sdp,
      revision: 1,
      connection: offer.connection,
    });
    await flush();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.failed",
    });
    expect(pc.close).toHaveBeenCalledOnce();
    pc.connectionState = "failed";
    lateState?.();
    expect(pc.close).toHaveBeenCalledOnce();
  });

  it("drops removed roster legs and acknowledges the new host revision", async () => {
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 1,
    });
    await flush();
    expect(Peer.all).toHaveLength(2);
    signal("host", "roster", { devices: ["host", "local"], revision: 2 });
    await flush();
    expect(Peer.all[1].close).toHaveBeenCalledOnce();
    expect(Peer.all[0].close).not.toHaveBeenCalled();
    expect(sent()).toContainEqual({
      to: "host",
      packet: { group: "g", meeting: "meeting", kind: "ready", revision: 2 },
    });
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 1,
    });
    await flush();
    expect(Peer.all).toHaveLength(2);
  });
  it("bounds and expires authenticated announcements without opening media", async () => {
    for (let index = 0; index < 18; index++) {
      handleGroupVoiceSignal(
        {
          from: "host",
          payload: JSON.stringify({
            protocol: "group-voice-1",
            group: `g${index}`,
            meeting: "m",
            kind: "announce",
          }),
        },
        "local",
      );
      await flush();
    }
    expect(Object.keys(useGroupVoice.getState().invites)).toHaveLength(16);
    expect(useGroupVoice.getState().invites.g0).toBeUndefined();
    expect(useGroupVoice.getState().invites.g17).toMatchObject({
      host: "host",
      meeting: "m",
    });
    expect(navigator.mediaDevices.getUserMedia).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(120001);
    signal("host", "announce");
    await flush();
    expect(Object.keys(useGroupVoice.getState().invites)).toEqual(["g"]);
    membership.set("intruder", null);
    signal("intruder", "announce");
    await flush();
    expect(useGroupVoice.getState().invites.g.host).toBe("host");
  });

  it("removes revoked remote membership while preserving other active media", async () => {
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 1,
    });
    await flush();
    membership.set("peer", null);
    notifySyncInvalidation({ owner: "owner", kind: "state" });
    await flush();
    expect(Peer.all[1].close).toHaveBeenCalledOnce();
    expect(Peer.all[0].close).not.toHaveBeenCalled();
    expect(useGroupVoice.getState().active).not.toBeNull();
    membership.set("host", null);
    notifySyncInvalidation({ owner: "owner", kind: "state" });
    await flush();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.ended",
    });
    expect(track.stop).toHaveBeenCalledOnce();
  });

  it("bounds missing new-member offers and retains the existing host leg", async () => {
    await joined();
    Peer.all[0].connectionState = "connected";
    Peer.all[0].onconnectionstatechange?.();
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 2,
    });
    await flush();
    expect(Peer.all).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(20000);
    expect(useGroupVoice.getState().error).toBe("groupVoice.failed");
    expect(useGroupVoice.getState().active).not.toBeNull();
    expect(Peer.all[0].close).not.toHaveBeenCalled();
  });

  it("contains microphone acquisition failure and never announces an unusable room", async () => {
    vi.mocked(navigator.mediaDevices.getUserMedia).mockRejectedValueOnce(
      new Error("permission denied"),
    );
    await startGroupVoice("g", "local", "owner", ["host"]);
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.unable",
    });
    expect(sent()).toEqual([]);
  });
  it.each([
    null,
    [],
    { protocol: "wrong" },
    { protocol: "group-voice-1", group: "", meeting: "m", kind: "join" },
    { protocol: "group-voice-1", group: "g", meeting: "m", kind: "unknown" },
    {
      protocol: "group-voice-1",
      group: "g",
      meeting: "m",
      kind: "ready",
      revision: -1,
    },
    {
      protocol: "group-voice-1",
      group: "g",
      meeting: "m",
      kind: "offer",
      revision: 1,
      connection: "c",
      sdp: "missing media",
    },
  ])("rejects malformed control packets %j", (value) => {
    expect(parseGroupVoice(JSON.stringify(value))).toBeNull();
  });
  it("rejects malformed JSON and oversized packets before signaling", () => {
    expect(parseGroupVoice("{")).toBeNull();
    expect(parseGroupVoice("x".repeat(32769))).toBeNull();
    expect(
      handleGroupVoiceSignal({ from: "host", payload: "{" }, "local"),
    ).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });
  it.each(["failed", "closed"])(
    "retires a %s media connection immediately",
    async (state) => {
      await joined();
      const pc = Peer.all[0];
      pc.connectionState = state;
      pc.onconnectionstatechange?.();
      expect(pc.close).toHaveBeenCalledOnce();
      expect(useGroupVoice.getState()).toMatchObject({
        active: null,
        error: "groupVoice.failed",
      });
    },
  );
  it("times out an unestablished media leg", async () => {
    await joined();
    await vi.advanceTimersByTimeAsync(20000);
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.failed",
    });
  });
  it("closes a partially created leg if installing microphone tracks fails", async () => {
    vi.stubGlobal(
      "RTCPeerConnection",
      class extends Peer {
        constructor() {
          super();
          this.addTrack.mockImplementation(() => {
            throw new Error("device lost");
          });
        }
      },
    );
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", { devices: ["host", "local"], revision: 1 });
    await flush();
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: "groupVoice.failed",
    });
  });
  it("ignores packets for another meeting, group or local identity", async () => {
    signal("host", "leave");
    await flush();
    await joined();
    signal("host", "end", {}, "other");
    for (const [group, self] of [
      ["other", "local"],
      ["g", "other"],
    ])
      handleGroupVoiceSignal(
        {
          from: "host",
          payload: JSON.stringify({
            protocol: "group-voice-1",
            group,
            meeting: "meeting",
            kind: "end",
          }),
        },
        self,
      );
    signal("local", "end");
    await flush();
    expect(useGroupVoice.getState().active).not.toBeNull();
    expect(Peer.all[0].close).not.toHaveBeenCalled();
  });
  it("revalidates a joining account after existing peers acknowledge", async () => {
    await startGroupVoice("g", "local", "owner", ["a", "b"]);
    const meeting = sent()[0]?.packet?.meeting;
    signal("a", "join", {}, meeting);
    await flush();
    signal("b", "join", {}, meeting);
    await flush();
    membership.set("b", null);
    signal("a", "ready", { revision: 2 }, meeting);
    await flush();
    expect(
      sent().some((item) => item.to === "b" && item.packet?.kind === "roster"),
    ).toBe(false);
    expect(sent()).toContainEqual({
      to: "a",
      packet: {
        group: "g",
        meeting,
        kind: "roster",
        revision: 3,
        devices: ["local", "a"],
      },
    });
  });
  it("rolls back admission if a prior member leaves during acknowledgement", async () => {
    await startGroupVoice("g", "local", "owner", ["a", "b"]);
    const meeting = sent()[0]?.packet?.meeting;
    signal("a", "join", {}, meeting);
    await flush();
    signal("b", "join", {}, meeting);
    await flush();
    signal("a", "leave", {}, meeting);
    await flush();
    signal("a", "ready", { revision: 2 }, meeting);
    await flush();
    expect(
      sent().some((item) => item.to === "b" && item.packet?.kind === "roster"),
    ).toBe(false);
    expect(useGroupVoice.getState().active?.host).toBe(true);
  });
  it.each([1, 2])(
    "cancels admission during membership lookup %i before microphone capture",
    async (lookup) => {
      let finish!: (value: string) => void;
      let count = 0;
      vi.mocked(invoke).mockImplementation(async (command) => {
        if (command !== "group_voice_member") return;
        if (++count === lookup)
          return new Promise<string>((resolve) => {
            finish = resolve;
          });
        return "account";
      });
      const joining = joinGroupVoice(invite(), "local", "owner");
      await flush();
      stopGroupVoice();
      finish("account");
      await joining;
      expect(navigator.mediaDevices.getUserMedia).not.toHaveBeenCalled();
      expect(useGroupVoice.getState().active).toBeNull();
    },
  );
  it("does not announce to unauthorized peers or continue announcements after stop", async () => {
    let finish!: (value: string) => void;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command !== "group_voice_member") return;
      if (fields(args).deviceId === "denied") return null;
      if (fields(args).deviceId === "pending")
        return new Promise<string>((resolve) => {
          finish = resolve;
        });
      return "account";
    });
    const starting = startGroupVoice("g", "local", "owner", [
      "denied",
      "pending",
      "later",
    ]);
    await flush();
    stopGroupVoice();
    finish("account");
    await starting;
    expect(sent()).toEqual([]);
    expect(track.stop).toHaveBeenCalledOnce();
  });
  it("contains a failed best-effort announcement", async () => {
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "group_voice_member") return "account";
      throw new Error("offline");
    });
    await startGroupVoice("g", "local", "owner", ["host"]);
    expect(useGroupVoice.getState().active).toMatchObject({
      host: true,
      joining: false,
    });
  });
  it("rejects a signal from a revoked sender and a pending check after stop", async () => {
    await joined();
    membership.set("host", null);
    signal("host", "end");
    await flush();
    expect(useGroupVoice.getState().active).not.toBeNull();
    let finish!: (value: string) => void;
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === "group_voice_member"
        ? new Promise<string>((resolve) => {
            finish = resolve;
          })
        : undefined,
    );
    signal("host", "end");
    await flush();
    stopGroupVoice();
    finish("account:host");
    await flush();
    expect(track.stop).toHaveBeenCalledOnce();
  });
  it("keeps cancellation authoritative over an in-flight offer failure", async () => {
    let reject!: (error: Error) => void;
    vi.stubGlobal(
      "RTCPeerConnection",
      class extends Peer {
        constructor() {
          super();
          this.createOffer.mockImplementation(
            () =>
              new Promise((_resolve, failed) => {
                reject = failed;
              }),
          );
        }
      },
    );
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", { devices: ["host", "local"], revision: 1 });
    await flush();
    stopGroupVoice();
    reject(new Error("cancelled"));
    await flush();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: null,
    });
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
  });
  it("does not send a prepared offer after the room is stopped", async () => {
    let finish!: (value: { type: string; sdp: string }) => void;
    vi.stubGlobal(
      "RTCPeerConnection",
      class extends Peer {
        constructor() {
          super();
          this.createOffer.mockImplementation(
            () =>
              new Promise((resolve) => {
                finish = resolve;
              }),
          );
        }
      },
    );
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", { devices: ["host", "local"], revision: 1 });
    await flush();
    stopGroupVoice();
    finish({ type: "offer", sdp });
    await flush();
    expect(sent().some((item) => item.packet?.kind === "offer")).toBe(false);
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
  });
  it("ignores a late capture rejection after cancellation", async () => {
    let reject!: (error: Error) => void;
    vi.mocked(navigator.mediaDevices.getUserMedia).mockImplementation(
      () =>
        new Promise((_resolve, failed) => {
          reject = failed;
        }),
    );
    const joining = joinGroupVoice(invite(), "local", "owner");
    await flush();
    stopGroupVoice();
    reject(new Error("cancelled permission"));
    await joining;
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: null,
    });
  });
  it("coalesces document invalidations during a membership refresh", async () => {
    await joined();
    let finish!: (value: string) => void;
    let delayed = true;
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      if (command !== "group_voice_member") return;
      if (fields(args).deviceId === "local" && delayed) {
        delayed = false;
        return new Promise<string>((resolve) => {
          finish = resolve;
        });
      }
      return `account:${fields(args).deviceId}`;
    });
    notifySyncInvalidation({ owner: "owner", kind: "state" });
    await flush();
    notifySyncInvalidation({ owner: "owner", kind: "state" });
    notifySyncInvalidation({ owner: "owner", kind: "state" });
    const before = vi.mocked(invoke).mock.calls.length;
    finish("account:local");
    await flush();
    expect(vi.mocked(invoke).mock.calls.length - before).toBe(2);
    expect(useGroupVoice.getState().active).not.toBeNull();
  });
  it("does not restore a departed host through a later roster", async () => {
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 1,
    });
    await flush();
    signal("host", "leave");
    await flush();
    expect(useGroupVoice.getState().active).not.toBeNull();
    signal("host", "roster", {
      devices: ["host", "local", "peer"],
      revision: 2,
    });
    await flush();
    expect(
      sent().some(
        (item) => item.packet?.kind === "ready" && item.packet.revision === 2,
      ),
    ).toBe(false);
    expect(Peer.all[0].close).toHaveBeenCalledOnce();
    expect(Peer.all[1].close).not.toHaveBeenCalled();
  });
  it("cancels pending roster admission on stop without publishing a rollback", async () => {
    await startGroupVoice("g", "local", "owner", ["a", "b"]);
    const meeting = sent()[0]?.packet?.meeting;
    signal("a", "join", {}, meeting);
    await flush();
    signal("b", "join", {}, meeting);
    await flush();
    stopGroupVoice();
    await flush();
    expect(
      sent().some(
        (item) => item.packet?.kind === "roster" && item.packet.revision === 3,
      ),
    ).toBe(false);
    expect(track.stop).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState().active).toBeNull();
  });
  it("does not publish a gathered offer when cancellation wins before its continuation", async () => {
    vi.stubGlobal(
      "RTCPeerConnection",
      class extends Peer {
        constructor() {
          super();
          this.iceGatheringState = "gathering";
        }
      },
    );
    await joinGroupVoice(invite(), "local", "owner");
    signal("host", "roster", { devices: ["host", "local"], revision: 1 });
    await flush();
    const pc = Peer.all[0];
    expect(pc.localDescription).not.toBeNull();
    pc.iceGatheringState = "complete";
    pc.dispatchEvent(new Event("icegatheringstatechange"));
    stopGroupVoice();
    await flush();
    expect(sent().some((item) => item.packet?.kind === "offer")).toBe(false);
    expect(pc.close).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState()).toMatchObject({
      active: null,
      error: null,
    });
  });
});
