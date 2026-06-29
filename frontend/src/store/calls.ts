import { create } from "zustand";
import { calls } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { useAuth } from "@/store/auth";
import { useSettings } from "@/store/settings";
import type { CallSignalEvent } from "@/lib/types";

/**
 * 1:1 voice/video calling over WebRTC, signaled across the mesh (see lib/api `calls`).
 *
 * Topology: LAN, peers know each other's IPs, so the RTCPeerConnection uses ONLY host
 * candidates (`iceServers: []`) — no STUN/TURN. ICE is NON-trickle: we wait for gathering
 * to finish (instant on a LAN) and bundle all candidates into the one offer/answer, so each
 * call is just a couple of signaling sends rather than a Noise dial per candidate.
 *
 * Security: the media leg is DTLS-SRTP (mandatory in WebRTC), but that only protects bytes
 * in flight. Its MITM resistance rests on the DTLS fingerprint (carried inside the SDP)
 * arriving over an AUTHENTICATED channel. The backend binds every inbound signal's `from`
 * to the Noise-authenticated peer identity, and we only ever `setRemoteDescription` with an
 * SDP whose `from` matches the call's peer — so the fingerprint we trust is the real peer's.
 */

type ByeReason = "hangup" | "decline" | "busy" | "failed";

type Signal =
  | { callId: string; kind: "offer"; sdp: string; name: string; video: boolean }
  | { callId: string; kind: "answer"; sdp: string }
  | { callId: string; kind: "bye"; reason: ByeReason };

export type CallPhase =
  | "idle"
  | "outgoing"
  | "incoming"
  | "connecting"
  | "connected"
  | "ended";

export interface CallTarget {
  name: string;
  /** The account being called (for the glyph + display). */
  accountId: string;
  /** Every discovered device of that account. We ring them ALL; the first to answer wins
   * and the rest are cancelled — a contact signed in on several devices rings everywhere. */
  deviceIds: string[];
}

interface CallState {
  phase: CallPhase;
  peerId: string | null;
  peerName: string;
  callId: string | null;
  /** Whether this call was placed as a VIDEO call (vs a voice-only call). */
  video: boolean;
  localStream: MediaStream | null;
  remoteStream: MediaStream | null;
  micOn: boolean;
  camOn: boolean;
  /** Whether the LOCAL capture has a video track (false = audio-only fallback). */
  hasVideo: boolean;
  /** A terminal note for the UI (e.g. "declined", "busy", or an error), shown then cleared. */
  endedReason: string | null;
  error: string | null;

  startCall: (target: CallTarget, video: boolean) => Promise<void>;
  accept: () => Promise<void>;
  decline: () => void;
  hangup: () => void;
  toggleMic: () => void;
  toggleCam: () => void;
  onSignal: (e: CallSignalEvent) => void;
  /** Dismiss the transient end-of-call note (declined / busy / failed). */
  clearEnded: () => void;
  /** Stop capture + tear down any active call (called on logout). */
  teardown: () => void;
}

// Non-reactive engine handles (one call at a time), kept out of store state so they don't
// trigger re-renders and so we never serialize a live RTCPeerConnection.
let pc: RTCPeerConnection | null = null;
let pendingOffer: { sdp: string; video: boolean } | null = null;
let connectTimer: ReturnType<typeof setTimeout> | null = null;
/** While an OUTGOING call is ringing, the devices still being rung (we offered to all of the
 * account's devices). Emptied once one answers (it becomes the locked `peerId`) or the call
 * ends. Always empty for an incoming call. */
let ringingPeers: string[] = [];

const CONNECT_TIMEOUT_MS = 20_000;
const GATHER_TIMEOUT_MS = 2_000;

function send(target: string, msg: Signal): Promise<void> {
  return calls.signal(target, JSON.stringify(msg)).catch(() => {});
}

/** Stop every track of a stream (releases the camera/mic). */
function stopStream(s: MediaStream) {
  for (const t of s.getTracks()) t.stop();
}

/** Acquire mic + camera, falling back to audio-only (e.g. no camera, or a webview whose
 * getUserMedia can't open video). Throws only if even audio can't be captured. */
async function getLocalMedia(
  video: boolean,
): Promise<{ stream: MediaStream; hasVideo: boolean }> {
  try {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: true,
      video,
    });
    return { stream, hasVideo: stream.getVideoTracks().length > 0 };
  } catch {
    // Fall back to audio-only (no camera, or a webview that can't open video).
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    return { stream, hasVideo: false };
  }
}

/** Wait until ICE gathering completes (host-only candidates finish near-instantly on a LAN),
 * with a fallback timeout so a webview that never flips to "complete" can't hang the call. */
function gatherComplete(conn: RTCPeerConnection): Promise<void> {
  if (conn.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      conn.removeEventListener("icegatheringstatechange", check);
      resolve();
    };
    const check = () => {
      if (conn.iceGatheringState === "complete") done();
    };
    conn.addEventListener("icegatheringstatechange", check);
    setTimeout(done, GATHER_TIMEOUT_MS);
  });
}

export const useCalls = create<CallState>((set, get) => {
  /** Build a peer connection wired to surface the remote stream + connection state. */
  function newPc(): RTCPeerConnection {
    const conn = new RTCPeerConnection({ iceServers: [] });
    conn.ontrack = (e) => {
      if (e.streams[0]) set({ remoteStream: e.streams[0] });
    };
    conn.onconnectionstatechange = () => {
      const s = conn.connectionState;
      if (s === "connected") {
        if (connectTimer) clearTimeout(connectTimer);
        connectTimer = null;
        set({ phase: "connected" });
      } else if (s === "failed") {
        get().hangup();
      }
    };
    return conn;
  }

  function cleanup() {
    if (connectTimer) clearTimeout(connectTimer);
    connectTimer = null;
    if (pc) {
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.close();
      pc = null;
    }
    pendingOffer = null;
    ringingPeers = [];
    const { localStream } = get();
    if (localStream) for (const t of localStream.getTracks()) t.stop();
  }

  /** Reset to idle, stopping media. `reason` is surfaced briefly to the UI. */
  function end(reason: string | null) {
    cleanup();
    set({
      phase: "idle",
      peerId: null,
      peerName: "",
      callId: null,
      video: true,
      localStream: null,
      remoteStream: null,
      micOn: true,
      camOn: true,
      hasVideo: false,
      endedReason: reason,
    });
  }

  function armConnectTimeout() {
    if (connectTimer) clearTimeout(connectTimer);
    connectTimer = setTimeout(() => {
      if (get().phase === "connecting" || get().phase === "outgoing") {
        end("failed");
      }
    }, CONNECT_TIMEOUT_MS);
  }

  return {
    phase: "idle",
    peerId: null,
    peerName: "",
    callId: null,
    video: true,
    localStream: null,
    remoteStream: null,
    micOn: true,
    camOn: true,
    hasVideo: false,
    endedReason: null,
    error: null,

    startCall: async (target, video) => {
      if (get().phase !== "idle") return; // one call at a time
      if (target.deviceIds.length === 0) return;
      const callId = crypto.randomUUID();
      ringingPeers = [...target.deviceIds];
      set({
        phase: "outgoing",
        // A representative device for the glyph/roster lookup until one answers and locks in.
        peerId: target.deviceIds[0],
        peerName: target.name,
        callId,
        video,
        endedReason: null,
        error: null,
      });
      try {
        const { stream, hasVideo } = await getLocalMedia(video);
        // The user may have hung up / logged out while the camera was opening. If so the
        // call was already torn down; release the capture we just acquired and bail, or it
        // leaks (camera light stays on) and we'd dial peers we abandoned.
        if (get().callId !== callId) {
          stopStream(stream);
          return;
        }
        set({ localStream: stream, hasVideo, camOn: hasVideo });
        // Use a local handle for negotiation so a concurrent cleanup() (which nulls the
        // module `pc`) can't turn these awaits into a null deref.
        const conn = newPc();
        pc = conn;
        for (const t of stream.getTracks()) conn.addTrack(t, stream);
        const offer = await conn.createOffer();
        await conn.setLocalDescription(offer);
        await gatherComplete(conn);
        if (get().callId !== callId) return; // torn down mid-negotiation; cleanup() already ran
        const myName = useAuth.getState().user?.display_name ?? "";
        const payload = JSON.stringify({
          callId,
          kind: "offer",
          sdp: conn.localDescription?.sdp ?? "",
          name: myName,
          video,
        });
        // Ring every device of the account. NOT best-effort: if EVERY device is unreachable
        // the node errors on all of them, and we surface that at once rather than waiting out
        // the 20s timeout. As long as one offer is delivered, the call proceeds.
        const results = await Promise.allSettled(
          target.deviceIds.map((d) => calls.signal(d, payload)),
        );
        if (get().callId !== callId) return;
        if (results.every((r) => r.status === "rejected")) {
          end("failed");
          return;
        }
        armConnectTimeout();
      } catch (e) {
        set({ error: errorMessage(e) });
        end("failed");
      }
    },

    accept: async () => {
      const { phase, peerId, callId } = get();
      if (phase !== "incoming" || !pendingOffer || !peerId || !callId) return;
      const offer = pendingOffer;
      pendingOffer = null;
      try {
        // Mirror the caller's media kind so the answer's m-lines match the offer
        // (non-trickle, single-shot — a mismatched track would need renegotiation).
        const { stream, hasVideo } = await getLocalMedia(offer.video);
        // The caller may have hung up (a `bye` resets us to idle) while the camera was
        // opening. If so, release the capture and bail rather than answering a dead call.
        if (get().callId !== callId) {
          stopStream(stream);
          return;
        }
        set({
          localStream: stream,
          hasVideo,
          camOn: hasVideo,
          phase: "connecting",
        });
        const conn = newPc();
        pc = conn;
        for (const t of stream.getTracks()) conn.addTrack(t, stream);
        await conn.setRemoteDescription({ type: "offer", sdp: offer.sdp });
        const answer = await conn.createAnswer();
        await conn.setLocalDescription(answer);
        await gatherComplete(conn);
        if (get().callId !== callId) return; // torn down mid-negotiation; cleanup() already ran
        await calls.signal(
          peerId,
          JSON.stringify({
            callId,
            kind: "answer",
            sdp: conn.localDescription?.sdp ?? "",
          }),
        );
        armConnectTimeout();
      } catch (e) {
        set({ error: errorMessage(e) });
        void send(peerId, { callId, kind: "bye", reason: "failed" });
        end("failed");
      }
    },

    decline: () => {
      const { phase, peerId, callId } = get();
      if (phase !== "incoming") return;
      if (peerId && callId)
        void send(peerId, { callId, kind: "bye", reason: "decline" });
      end(null);
    },

    hangup: () => {
      const { phase, peerId, callId } = get();
      if (phase === "idle") return;
      if (callId) {
        // Cancel everyone we're still ringing (outgoing fan-out), else the single
        // connected/locked peer (connected call or incoming).
        const targets =
          ringingPeers.length > 0 ? ringingPeers : peerId ? [peerId] : [];
        for (const d of targets)
          void send(d, { callId, kind: "bye", reason: "hangup" });
      }
      end(null);
    },

    toggleMic: () => {
      const { localStream, micOn } = get();
      if (!localStream) return;
      const next = !micOn;
      for (const t of localStream.getAudioTracks()) t.enabled = next;
      set({ micOn: next });
    },

    toggleCam: () => {
      const { localStream, camOn, hasVideo } = get();
      if (!localStream || !hasVideo) return;
      const next = !camOn;
      for (const t of localStream.getVideoTracks()) t.enabled = next;
      set({ camOn: next });
    },

    onSignal: (e) => {
      let msg: Signal;
      try {
        msg = JSON.parse(e.payload) as Signal;
      } catch {
        return; // malformed signaling — ignore
      }
      const { phase, callId, peerId } = get();

      if (msg.kind === "offer") {
        // Opt-out: if the experimental calls feature is disabled, don't ring at all —
        // decline so the caller gets immediate feedback instead of a 20s timeout.
        if (!useSettings.getState().callsEnabled) {
          void send(e.from, {
            callId: msg.callId,
            kind: "bye",
            reason: "decline",
          });
          return;
        }
        // Busy: already in a call → tell the caller, don't disturb the active one.
        if (phase !== "idle") {
          void send(e.from, {
            callId: msg.callId,
            kind: "bye",
            reason: "busy",
          });
          return;
        }
        pendingOffer = { sdp: msg.sdp, video: msg.video };
        set({
          phase: "incoming",
          peerId: e.from,
          peerName: msg.name || "",
          callId: msg.callId,
          video: msg.video,
          endedReason: null,
          error: null,
        });
        return;
      }

      // answer / bye only apply to OUR current call id.
      if (msg.callId !== callId) return;

      if (msg.kind === "answer") {
        if (phase !== "outgoing" || !pc) return;
        // First device to answer wins. Accept an answer only from a device we're ringing
        // (authenticated `from`), lock the call to it, and cancel the other devices so they
        // stop ringing. The SDP's DTLS fingerprint is trustworthy because `from` is the
        // backend-authenticated peer, so applying it pins the media leg to the real device.
        if (!ringingPeers.includes(e.from)) return;
        const losers = ringingPeers.filter((d) => d !== e.from);
        ringingPeers = [];
        set({ peerId: e.from }); // lock the winner (also fixes the glyph/roster name)
        void pc
          .setRemoteDescription({ type: "answer", sdp: msg.sdp })
          .then(() => {
            set({ phase: "connecting" });
            for (const d of losers)
              void send(d, { callId, kind: "bye", reason: "hangup" });
          })
          .catch(() => end("failed"));
        return;
      }

      if (msg.kind === "bye") {
        // Surface only a peer-driven reason (declined / busy); hangup/failed end quietly.
        const note =
          msg.reason === "decline" || msg.reason === "busy" ? msg.reason : null;
        // A still-ringing device declined / was busy / unreachable: drop just that device,
        // and only end the call once NONE remain (the others may still pick up).
        if (phase === "outgoing" && ringingPeers.includes(e.from)) {
          ringingPeers = ringingPeers.filter((d) => d !== e.from);
          if (ringingPeers.length === 0) end(note);
          return;
        }
        // Otherwise it's the connected / locked peer (or the caller, for an incoming call)
        // hanging up — end the call.
        if (e.from === peerId) end(note);
      }
    },

    clearEnded: () => set({ endedReason: null, error: null }),

    teardown: () => end(null),
  };
});
