import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  handleGroupVoiceSignal,
  parseGroupVoice,
  startGroupVoice,
  stopGroupVoice,
  useGroupVoice,
} from "./groupVoice";
import { useSettings } from "./settings";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => {
  stopGroupVoice();
  useSettings.setState({ callsEnabled: false });
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("group voice signaling", () => {
  const packet = { protocol: "group-voice-1", group: "g", meeting: "m" };
  it("keeps direct calls on their existing signaling path", () => {
    expect(
      parseGroupVoice(JSON.stringify({ kind: "offer", sdp: "sdp" })),
    ).toBeNull();
  });
  it("does not contact the backend or acquire capture while disabled", async () => {
    const capture = vi.fn();
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: capture } });
    useSettings.setState({ callsEnabled: false });
    handleGroupVoiceSignal(
      {
        from: "peer",
        payload: JSON.stringify({ ...packet, kind: "announce" }),
      },
      "local",
    );
    await startGroupVoice("g", "local", "owner", ["peer"]);
    expect(capture).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("requires current membership before microphone access", async () => {
    const capture = vi.fn();
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: capture } });
    useSettings.setState({ callsEnabled: true });
    vi.mocked(invoke).mockResolvedValue(null);
    await startGroupVoice("g", "local", "owner", []);
    expect(capture).not.toHaveBeenCalled();
    expect(useGroupVoice.getState().active).toBeNull();
  });

  it("stops a late microphone result after disabling the experiment", async () => {
    const stop = vi.fn();
    let finish!: (stream: unknown) => void;
    const capture = vi.fn(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: capture } });
    useSettings.setState({ callsEnabled: true });
    vi.mocked(invoke).mockResolvedValue("account");
    const start = startGroupVoice("g", "local", "owner", []);
    await vi.waitFor(() => expect(capture).toHaveBeenCalled());
    useSettings.setState({ callsEnabled: false });
    finish({ getTracks: () => [{ stop }] });
    await start;
    expect(stop).toHaveBeenCalledOnce();
    expect(useGroupVoice.getState().active).toBeNull();
  });

  it("waits for the current roster acknowledgement before admitting a new device", async () => {
    vi.stubGlobal("navigator", {
      mediaDevices: {
        getUserMedia: vi.fn().mockResolvedValue({
          getTracks: () => [],
          getAudioTracks: () => [],
        }),
      },
    });
    useSettings.setState({ callsEnabled: true });
    vi.mocked(invoke).mockImplementation(async (command, args) =>
      command === "group_voice_member" && args && "deviceId" in args
        ? `account:${args.deviceId}`
        : undefined,
    );
    await startGroupVoice("g", "local", "owner", ["a", "b", "c", "d"]);
    const sent = () =>
      vi
        .mocked(invoke)
        .mock.calls.flatMap(([command, args]) =>
          command === "send_call_signal" &&
          args &&
          "payload" in args &&
          "target" in args &&
          typeof args.payload === "string"
            ? [{ to: args.target, packet: parseGroupVoice(args.payload) }]
            : [],
        );
    const invite = sent()[0]?.packet;
    expect(invite).toMatchObject({ group: "g", kind: "announce" });
    const from = (device: string, kind: string, extra = {}) =>
      handleGroupVoiceSignal(
        {
          from: device,
          payload: JSON.stringify({
            protocol: "group-voice-1",
            group: "g",
            meeting: invite?.meeting,
            kind,
            ...extra,
          }),
        },
        "local",
      );
    const hasRoster = (to: string, revision: number) =>
      sent().some(
        (item) =>
          item.to === to &&
          item.packet?.kind === "roster" &&
          item.packet.revision === revision,
      );

    from("a", "join");
    await vi.waitFor(() => expect(hasRoster("a", 1)).toBe(true));
    from("b", "join");
    await vi.waitFor(() => expect(hasRoster("a", 2)).toBe(true));
    from("a", "ready", { revision: 1 });
    await Promise.resolve();
    await Promise.resolve();
    expect(hasRoster("b", 2)).toBe(false);
    from("a", "ready", { revision: 2 });
    await vi.waitFor(() => expect(hasRoster("b", 2)).toBe(true));

    from("c", "join");
    await vi.waitFor(() => expect(hasRoster("b", 3)).toBe(true));
    from("a", "ready", { revision: 3 });
    from("b", "ready", { revision: 3 });
    await vi.waitFor(() => expect(hasRoster("c", 3)).toBe(true));
    from("d", "join");
    await Promise.resolve();
    await Promise.resolve();
    expect(
      sent().some(
        (item) => item.packet?.kind === "roster" && item.packet.revision > 3,
      ),
    ).toBe(false);
  });

  it("admits only a single audio media section per negotiation", () => {
    const offer = {
      ...packet,
      kind: "offer",
      connection: "connection",
      revision: 1,
      sdp: "v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
    };
    expect(parseGroupVoice(JSON.stringify(offer))).toMatchObject({
      kind: "offer",
    });
    expect(
      parseGroupVoice(
        JSON.stringify({
          ...offer,
          sdp: offer.sdp + "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n",
        }),
      ),
    ).toBeNull();
  });

  it("bounds roster and requires a valid SDP message", () => {
    expect(
      parseGroupVoice(
        JSON.stringify({
          ...packet,
          kind: "roster",
          revision: 1,
          devices: ["a", "b", "c", "d", "e"],
        }),
      ),
    ).toBeNull();
    expect(
      parseGroupVoice(JSON.stringify({ ...packet, kind: "offer", sdp: [] })),
    ).toBeNull();
    expect(
      parseGroupVoice(JSON.stringify({ ...packet, kind: "join" })),
    ).toMatchObject({ kind: "join" });
  });
});
