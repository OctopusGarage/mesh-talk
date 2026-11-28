import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import { calls } from "@/lib/api/calls";
import { subscribeDocumentInvalidation } from "@/lib/syncInvalidation";
import type { CallSignalEvent } from "@/lib/types";
import { gatherComplete, useCalls } from "./calls";
import { useSettings } from "./settings";

const PROTOCOL = "group-voice-1";
const CAPACITY = 4;
const TIMEOUT = 20_000;
const ANNOUNCEMENT_LIMIT = 64;
type Kind =
  | "announce"
  | "join"
  | "ready"
  | "roster"
  | "offer"
  | "answer"
  | "leave"
  | "end";
type Packet = {
  group: string;
  meeting: string;
} & (
  | { kind: "announce" | "join" | "leave" | "end" }
  | { kind: "ready"; revision: number }
  | { kind: "roster"; devices: string[]; revision: number }
  | {
      kind: "offer" | "answer";
      sdp: string;
      connection: string;
      revision: number;
    }
);

export function parseGroupVoice(raw: string): Packet | null {
  if (raw.length > 32_768) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return null;
    const {
      protocol,
      group,
      meeting,
      kind,
      devices,
      sdp,
      revision,
      connection,
    } = value as Record<string, unknown>;
    const validId = (id: unknown): id is string =>
      typeof id === "string" && id.length > 0 && id.length <= 128;
    if (protocol !== PROTOCOL || !validId(group) || !validId(meeting))
      return null;
    const validRevision =
      typeof revision === "number" &&
      Number.isSafeInteger(revision) &&
      revision > 0;
    switch (kind) {
      case "announce":
      case "join":
      case "leave":
      case "end":
        return { group, meeting, kind };
      case "ready":
        return validRevision ? { group, meeting, kind, revision } : null;
      case "roster":
        return validRevision &&
          Array.isArray(devices) &&
          devices.length <= CAPACITY &&
          devices.every(validId) &&
          new Set(devices).size === devices.length
          ? { group, meeting, kind, devices, revision }
          : null;
      case "offer":
      case "answer": {
        const sections = typeof sdp === "string" ? sdp.match(/^m=.*$/gm) : null;
        return validRevision &&
          validId(connection) &&
          typeof sdp === "string" &&
          sections?.length === 1 &&
          sections[0].startsWith("m=audio ")
          ? { group, meeting, kind, sdp, connection, revision }
          : null;
      }
      default:
        return null;
    }
  } catch {
    return null;
  }
}

type Invite = { group: string; meeting: string; host: string; expires: number };
type Leg = {
  connection: string;
  revision: number;
  pc: RTCPeerConnection;
  abort: AbortController;
  timer: ReturnType<typeof setTimeout>;
};
type Session = {
  invite: Invite;
  self: string;
  microphone: MediaStream | null;
  roster: Map<string, string>;
  pending: Set<string>;
  canceledJoin?: string;
  revision: number;
  confirm?: (sender: string, revision: number) => void;
  cancelConfirm?: () => void;
  legs: Map<string, Leg>;
  expected: Map<
    string,
    { timer: ReturnType<typeof setTimeout>; revision: number }
  >;
  timer?: ReturnType<typeof setTimeout>;
  unsubscribe?: () => void;
};
type State = {
  active: { group: string; host: boolean; joining: boolean } | null;
  invites: Record<string, Invite>;
  audio: Record<string, MediaStream>;
  muted: boolean;
  error: string | null;
};
export const useGroupVoice = create<State>(() => ({
  active: null,
  invites: {},
  audio: {},
  muted: false,
  error: null,
}));
let session: Session | null = null;
let generation = 0;

function member(groupId: string, deviceId: string) {
  return invoke<string | null>("group_voice_member", {
    groupId,
    deviceId,
  }).catch(() => null);
}

function send(to: string, { group, meeting }: Invite, kind: Kind, extra = {}) {
  return calls.signal(
    to,
    JSON.stringify({ protocol: PROTOCOL, group, meeting, kind, ...extra }),
  );
}

function closeLeg(room: Session, device: string) {
  clearTimeout(room.expected.get(device)?.timer);
  room.expected.delete(device);
  const leg = room.legs.get(device);
  if (!leg) return;
  room.legs.delete(device);
  clearTimeout(leg.timer);
  leg.abort.abort();
  leg.pc.ontrack = null;
  leg.pc.onconnectionstatechange = null;
  leg.pc.close();
  useGroupVoice.setState(({ audio }) => ({
    audio: Object.fromEntries(
      Object.entries(audio).filter(([id]) => id !== device),
    ),
  }));
}

export function stopGroupVoice(notify: boolean = false, end: boolean = false) {
  const room = session;
  generation++;
  session = null;
  if (room) {
    if (notify)
      for (const device of new Set([...room.roster.keys(), room.invite.host]))
        if (device !== room.self)
          void send(
            device,
            room.invite,
            end && room.self === room.invite.host ? "end" : "leave",
          ).catch(() => {});
    room.unsubscribe?.();
    room.cancelConfirm?.();
    clearTimeout(room.timer);
    for (const device of room.expected.keys()) closeLeg(room, device);
    for (const device of room.legs.keys()) closeLeg(room, device);
    room.microphone?.getTracks().forEach((track) => track.stop());
  }
  useGroupVoice.setState({
    active: null,
    audio: {},
    muted: false,
    invites: {},
    error: null,
  });
}

function fail(room: Session, message: string) {
  if (session !== room) return;
  stopGroupVoice(true);
  useGroupVoice.setState({ error: message });
}

export function muteGroupVoice() {
  const muted = !useGroupVoice.getState().muted;
  session?.microphone?.getAudioTracks().forEach((track) => {
    track.enabled = !muted;
  });
  useGroupVoice.setState({ muted });
}

async function openRoom(invite: Invite, self: string, owner: string) {
  if (
    !self ||
    !owner ||
    session ||
    !useSettings.getState().callsEnabled ||
    useCalls.getState().phase !== "idle"
  )
    return null;
  const room: Session = {
    invite,
    self,
    microphone: null,
    roster: new Map(),
    revision: 0,
    pending: new Set(),
    legs: new Map(),
    expected: new Map(),
  };
  session = room;
  useGroupVoice.setState({
    active: { group: invite.group, host: invite.host === self, joining: true },
    error: null,
    muted: false,
  });
  try {
    const account = await member(invite.group, self);
    if (session !== room) return null;
    if (
      !account ||
      (invite.host !== self && !(await member(invite.group, invite.host)))
    )
      throw new Error("Admission denied");
    if (session !== room) return null;
    room.roster.set(self, account);
    const microphone = await navigator.mediaDevices.getUserMedia({
      audio: true,
    });
    if (session !== room) {
      microphone.getTracks().forEach((track) => track.stop());
      return null;
    }
    room.microphone = microphone;
    microphone.getAudioTracks().forEach((track) => {
      track.enabled = !useGroupVoice.getState().muted;
    });
    let checking = false;
    let dirty = false;
    const recheck = async () => {
      if (checking) return;
      checking = true;
      while (dirty && session === room) {
        dirty = false;
        const checks = [...room.roster].map(async ([device, accountId]) => {
          const actual = await member(invite.group, device);
          if (
            session !== room ||
            room.roster.get(device) !== accountId ||
            actual === accountId
          )
            return;
          if (device === self) {
            fail(room, "groupVoice.access");
          } else {
            room.roster.delete(device);
            closeLeg(room, device);
            closeIfAlone(room);
          }
        });
        await Promise.all(checks);
      }
      checking = false;
    };
    room.unsubscribe = subscribeDocumentInvalidation(owner, () => {
      dirty = true;
      void recheck();
    });
    return room;
  } catch {
    fail(room, "groupVoice.unable");
    return null;
  }
}

export async function startGroupVoice(
  group: string,
  self: string,
  owner: string,
  members: string[],
) {
  const invite = {
    group,
    host: self,
    meeting: crypto.randomUUID(),
    expires: 0,
  };
  const room = await openRoom(invite, self, owner);
  if (!room || session !== room) return;
  useGroupVoice.setState({ active: { group, host: true, joining: false } });
  // Limit concurrent signaling and avoid announcing to unverified devices.
  const targets = [
    ...new Set(members.filter((device) => device !== self)),
  ].slice(0, ANNOUNCEMENT_LIMIT);
  for (const device of targets) {
    if (session !== room) return;
    if ((await member(group, device)) && session === room)
      await send(device, invite, "announce").catch(() => {});
  }
}

export async function joinGroupVoice(
  invite: Invite,
  self: string,
  owner: string,
) {
  if (Date.now() >= invite.expires) {
    useGroupVoice.setState({
      error: "groupVoice.expired",
    });
    return;
  }
  const room = await openRoom(invite, self, owner);
  if (!room || session !== room) return;
  room.timer = setTimeout(() => fail(room, "groupVoice.denied"), TIMEOUT);
  try {
    await send(invite.host, invite, "join");
  } catch {
    fail(room, "groupVoice.unreachable");
  }
}

function expectOffer(room: Session, device: string, revision: number) {
  if (
    device !== room.self &&
    !room.expected.has(device) &&
    !room.legs.has(device)
  )
    room.expected.set(device, {
      revision,
      timer: setTimeout(() => legFailed(room, device), TIMEOUT),
    });
}

function makeLeg(
  room: Session,
  device: string,
  connection: string,
  revision: number,
) {
  if (!room.microphone || room.legs.has(device)) return null;
  clearTimeout(room.expected.get(device)?.timer);
  room.expected.delete(device);
  const pc = new RTCPeerConnection({ iceServers: [] });
  const abort = new AbortController();
  const leg: Leg = {
    connection,
    revision,
    pc,
    abort,
    timer: setTimeout(() => legFailed(room, device), TIMEOUT),
  };
  room.legs.set(device, leg);
  try {
    for (const track of room.microphone.getAudioTracks())
      pc.addTrack(track, room.microphone);
    pc.ontrack = (event) => {
      if (
        session !== room ||
        room.legs.get(device) !== leg ||
        event.track.kind !== "audio"
      )
        return;
      useGroupVoice.setState(({ audio }) => ({
        audio: {
          ...audio,
          [device]: new MediaStream([event.track]),
        },
      }));
    };
    pc.onconnectionstatechange = () => {
      if (session !== room || room.legs.get(device) !== leg) return;
      if (pc.connectionState === "connected") clearTimeout(leg.timer);
      if (pc.connectionState === "disconnected") {
        clearTimeout(leg.timer);
        leg.timer = setTimeout(() => legFailed(room, device), TIMEOUT);
      }
      if (pc.connectionState === "failed" || pc.connectionState === "closed")
        legFailed(room, device);
    };
    return leg;
  } catch (error) {
    closeLeg(room, device);
    throw error;
  }
}

function closeIfAlone(room: Session) {
  if (
    room.self !== room.invite.host &&
    room.roster.size === 1 &&
    !room.legs.size
  )
    fail(room, "groupVoice.ended");
}

function legFailed(room: Session, device: string) {
  if (session !== room) return;
  room.roster.delete(device);
  closeLeg(room, device);
  closeIfAlone(room);
  useGroupVoice.setState({ error: "groupVoice.failed" });
}

async function negotiate(
  room: Session,
  device: string,
  revision: number,
  offer?: { sdp: string; connection: string },
) {
  try {
    const connection = offer?.connection ?? crypto.randomUUID();
    const leg = makeLeg(room, device, connection, revision);
    if (!leg) return;
    const { pc, abort } = leg;
    if (offer) await pc.setRemoteDescription({ type: "offer", sdp: offer.sdp });
    await pc.setLocalDescription(
      offer ? await pc.createAnswer() : await pc.createOffer(),
    );
    const sdp = await gatherComplete(pc, abort.signal);
    if (session === room && room.legs.get(device) === leg)
      await send(device, room.invite, offer ? "answer" : "offer", {
        sdp,
        connection,
        revision,
      });
  } catch {
    legFailed(room, device);
  }
}

async function applyPacket(packet: Packet, from: string, self: string) {
  if (!self || from === self || !useSettings.getState().callsEnabled) return;
  const room = session;
  const turn = generation;
  if (packet.kind === "announce") {
    if (
      !room &&
      (await member(packet.group, from)) &&
      (await member(packet.group, self)) &&
      !session &&
      turn === generation &&
      useSettings.getState().callsEnabled
    ) {
      useGroupVoice.setState(({ invites }) => {
        const now = Date.now();
        const retained = Object.entries(invites)
          .filter(
            ([key, invite]) => key !== packet.group && invite.expires > now,
          )
          .slice(-15);
        return {
          invites: Object.fromEntries([
            ...retained,
            [
              packet.group,
              {
                group: packet.group,
                meeting: packet.meeting,
                host: from,
                expires: now + 120_000,
              },
            ],
          ]),
        };
      });
    }
    return;
  }
  if (
    !room ||
    room.self !== self ||
    room.invite.group !== packet.group ||
    room.invite.meeting !== packet.meeting
  )
    return;
  if (packet.kind === "leave" && room.pending.has(from)) {
    room.canceledJoin = from;
    room.cancelConfirm?.();
    return;
  }
  if (packet.kind === "leave" && room.roster.has(from)) {
    room.roster.delete(from);
    closeLeg(room, from);
    closeIfAlone(room);
    return;
  }
  const account = await member(packet.group, from);
  if (!account || session !== room) return;
  const host = room.invite.host;
  if (packet.kind === "end" && from === host && room.roster.has(from)) {
    stopGroupVoice();
  } else if (packet.kind === "ready" && self === host) {
    room.confirm?.(from, packet.revision);
  } else if (packet.kind === "join" && self === host) {
    if (
      room.roster.size + room.pending.size >= CAPACITY ||
      room.pending.size ||
      room.roster.has(from) ||
      [...room.roster.values()].includes(account)
    )
      return;
    room.pending.add(from);
    const existing = [...room.roster.keys()];
    const devices = [...existing, from];
    const revision = ++room.revision;
    try {
      const awaiting = new Set(existing.filter((device) => device !== self));
      if (awaiting.size) {
        const ready = new Promise<boolean>((resolve) => {
          const complete = (ok: boolean) => {
            clearTimeout(timer);
            room.confirm = undefined;
            room.cancelConfirm = undefined;
            resolve(ok);
          };
          const timer = setTimeout(() => complete(false), TIMEOUT / 2);
          room.cancelConfirm = () => complete(false);
          room.confirm = (sender, receivedRevision) => {
            if (receivedRevision !== revision) return;
            awaiting.delete(sender);
            if (!awaiting.size) complete(true);
          };
        });
        try {
          await Promise.all(
            [...awaiting].map((device) =>
              send(device, room.invite, "roster", { devices, revision }),
            ),
          );
          if (!(await ready)) throw new Error("Roster confirmation timed out");
        } finally {
          room.cancelConfirm?.();
        }
      }
      if (
        session !== room ||
        (await member(packet.group, from)) !== account ||
        session !== room
      )
        throw new Error("Participant no longer authorized");
      if (
        room.canceledJoin === from ||
        existing.some((id) => !room.roster.has(id))
      )
        throw new Error("Meeting roster changed");
      room.roster.set(from, account);
      expectOffer(room, from, revision);
      await send(from, room.invite, "roster", { devices, revision });
    } catch {
      room.roster.delete(from);
      closeLeg(room, from);
      if (session === room) {
        const rollbackRevision = ++room.revision;
        for (const device of room.roster.keys())
          if (device !== self)
            void send(device, room.invite, "roster", {
              devices: [...room.roster.keys()],
              revision: rollbackRevision,
            }).catch(() => {});
      }
    } finally {
      room.pending.delete(from);
      room.canceledJoin = undefined;
    }
  } else if (
    packet.kind === "roster" &&
    from === host &&
    packet.revision > room.revision &&
    packet.devices.includes(self) &&
    packet.devices.includes(host)
  ) {
    const entries = await Promise.all(
      packet.devices.map(
        async (device) => [device, await member(packet.group, device)] as const,
      ),
    );
    if (
      session !== room ||
      packet.revision <= room.revision ||
      entries.some(([, id]) => id === null) ||
      new Set(entries.map(([, id]) => id)).size !== entries.length
    )
      return;
    const initial = useGroupVoice.getState().active?.joining;
    if (!initial && !room.roster.has(host)) return;
    room.revision = packet.revision;
    for (const device of room.roster.keys()) {
      if (!packet.devices.includes(device)) {
        room.roster.delete(device);
        closeLeg(room, device);
      }
    }
    for (const [device, id] of entries) {
      if (!initial && !room.roster.has(device))
        expectOffer(room, device, packet.revision);
      if (id) room.roster.set(device, id);
    }
    if (!initial)
      void send(host, room.invite, "ready", {
        revision: packet.revision,
      }).catch(() => {});
    clearTimeout(room.timer);
    useGroupVoice.setState({
      active: { group: packet.group, host: false, joining: false },
    });
    // The joining device alone offers to existing members.
    if (initial)
      for (const device of packet.devices)
        if (device !== self) void negotiate(room, device, packet.revision);
  } else if (room.roster.get(from) === account && packet.kind === "offer") {
    if (room.expected.get(from)?.revision === packet.revision)
      await negotiate(room, from, packet.revision, packet);
  } else if (room.roster.get(from) === account && packet.kind === "answer") {
    const leg = room.legs.get(from);
    if (
      leg?.connection === packet.connection &&
      leg.revision === packet.revision &&
      leg.pc.signalingState === "have-local-offer"
    ) {
      try {
        await leg.pc.setRemoteDescription({ type: "answer", sdp: packet.sdp });
      } catch {
        legFailed(room, from);
      }
    }
  }
}

export function handleGroupVoiceSignal(event: CallSignalEvent, self: string) {
  const packet = parseGroupVoice(event.payload);
  if (!packet) return false;
  void applyPacket(packet, event.from, self).catch(() => {});
  return true;
}

useSettings.subscribe((state, previous) => {
  if (previous.callsEnabled && !state.callsEnabled) stopGroupVoice();
});
