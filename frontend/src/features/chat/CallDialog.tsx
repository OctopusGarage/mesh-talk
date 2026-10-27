import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import {
  FlaskConical,
  Mic,
  MicOff,
  Phone,
  PhoneIncoming,
  PhoneOff,
  Video as VideoIcon,
  VideoOff,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { IdentityGlyph } from "@/components/identity";
import { usePresenceFor } from "@/store/presence";
import { useCalls, type CallPhase } from "@/store/calls";
import { useChat } from "@/store/chat";
import { useSettings } from "@/store/settings";
import { shortId } from "@/lib/format";
import { playCallTone } from "@/lib/ringtones";
import { cn } from "@/lib/utils";
import { rosterProjection } from "@/lib/rosterProjection";
import { ease, useMotionOK } from "@/lib/motion";
import { useAuth } from "@/store/auth";
import { CallConnectionNotice } from "./CallConnectionNotice";

/** Explicit false means mailbox-only; older hosts without this field remain attemptable. */
function callDeviceIds(accountId: string) {
  return rosterProjection(useChat.getState().peers)
    .contactDevicesForAccount(accountId)
    .filter((peer) => peer.directly_reachable !== false)
    .map((peer) => peer.user_id);
}

/** Header actions: start a voice or video call with a DM contact. Separate entry points
 * (like WeChat et al.). Rendered only when the experimental calls feature is enabled, and
 * each is enabled only when a device of that account is discovered (online) and idle.
 * The flask marks the feature as experimental / unreliable. */
export function CallButtons({
  accountId,
  name,
}: {
  accountId: string;
  name: string;
}) {
  const { t } = useTranslation();
  const callsEnabled = useSettings((s) => s.callsEnabled);
  const phase = useCalls((s) => s.phase);
  const presence = usePresenceFor(accountId);
  const online = presence.online;
  const [unavailable, setUnavailable] = useState(false);
  const lastTrigger = useRef<HTMLButtonElement | null>(null);
  // Subscribe to a BOOLEAN (does the account have any discovered device?), not the whole
  // roster array — the array's identity changes every ~4s on the roster poll and would
  // re-render the header for nothing.
  const hasDevice = useChat(
    (s) =>
      rosterProjection(s.peers).contactDevicesForAccount(accountId).length > 0,
  );
  // Opt-in: the feature is hidden entirely until enabled in Settings.
  if (!callsEnabled) return null;
  const canCall = phase === "idle" && online && hasDevice;
  const start = (video: boolean, trigger: HTMLButtonElement) => {
    lastTrigger.current = trigger;
    // Read the device list fresh at click time. Ring EVERY discovered device of the account
    // (first-to-answer wins; see store/calls.ts).
    const deviceIds = callDeviceIds(accountId);
    if (!deviceIds.length) {
      setUnavailable(true);
      return;
    }
    void useCalls.getState().startCall({ name, accountId, deviceIds }, video);
  };
  return (
    <>
      <div className="flex items-center gap-0.5">
        <span
          className="text-amber-500"
          title={t("call.experimentalHint")}
          aria-label={t("call.experimental")}
        >
          <FlaskConical className="h-3.5 w-3.5" />
        </span>
        <Button
          variant="ghost"
          size="icon"
          disabled={!canCall}
          data-testid="conversation-call-voice"
          title={canCall ? t("call.voice") : t("call.offlineHint")}
          onClick={(event) => start(false, event.currentTarget)}
        >
          <Phone className="h-4 w-4" />
        </Button>
        <Button
          variant="ghost"
          size="icon"
          disabled={!canCall}
          data-testid="conversation-call-video"
          title={canCall ? t("call.video") : t("call.offlineHint")}
          onClick={(event) => start(true, event.currentTarget)}
        >
          <VideoIcon className="h-4 w-4" />
        </Button>
      </div>
      <CallConnectionNotice
        open={unavailable}
        onOpenChange={setUnavailable}
        unavailable
        returnFocus={() => lastTrigger.current?.focus()}
      />
    </>
  );
}

/** A `<video>` bound to a MediaStream via srcObject (which can't be set declaratively). */
function StreamVideo({
  stream,
  muted,
  className,
}: {
  stream: MediaStream | null;
  muted?: boolean;
  className?: string;
}) {
  const ref = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    const player = ref.current;
    if (!player) return;
    player.srcObject = stream;
    // `autoPlay` alone isn't enough on autoplay-restricted webviews (WKWebView/WebKitGTK):
    // without an explicit play() the remote video/audio can stay paused though the stream
    // is flowing. Best-effort — a rejection (no user gesture yet) is harmless.
    if (stream) void player.play().catch(() => {});
    return () => {
      player.pause();
      player.srcObject = null;
    };
  }, [stream]);
  return (
    <video ref={ref} autoPlay playsInline muted={muted} className={className} />
  );
}

/** Ring while a call is incoming/outgoing, using the user's chosen ringtone (see
 * lib/ringtones.ts). The caller always hears the standard ringback. */
function useCallTone(phase: CallPhase) {
  const ringtone = useSettings((s) => s.ringtone);
  useEffect(() => {
    if (phase !== "incoming" && phase !== "outgoing") return;
    return playCallTone(phase, ringtone);
  }, [phase, ringtone]);
}

const STATUS_KEY: Record<CallPhase, string> = {
  idle: "",
  outgoing: "call.calling",
  incoming: "call.incoming",
  connecting: "call.connecting",
  connected: "call.inCall",
  ended: "",
};

/** The call overlay: renders whenever a call is active (and a brief toast after one ends).
 * Mounted once at the app root. */
export function CallDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const phase = useCalls((s) => s.phase);
  const peerName = useCalls((s) => s.peerName);
  const peerId = useCalls((s) => s.peerId);
  // Prefer the name from our own roster (keyed by the AUTHENTICATED device id) over the
  // caller's self-asserted offer name. Select the derived NAME (a stable string), not the
  // roster array, so the always-mounted overlay doesn't re-render on every ~4s roster poll.
  const rosterName = useChat((s) =>
    peerId ? rosterProjection(s.peers).nameForDevice(peerId) : null,
  );
  const localStream = useCalls((s) => s.localStream);
  const remoteStream = useCalls((s) => s.remoteStream);
  const micOn = useCalls((s) => s.micOn);
  const camOn = useCalls((s) => s.camOn);
  const hasVideo = useCalls((s) => s.hasVideo);
  const video = useCalls((s) => s.video);
  const endedReason = useCalls((s) => s.endedReason);
  const error = useCalls((s) => s.error);
  const accept = useCalls((s) => s.accept);
  const decline = useCalls((s) => s.decline);
  const hangup = useCalls((s) => s.hangup);
  const toggleMic = useCalls((s) => s.toggleMic);
  const toggleCam = useCalls((s) => s.toggleCam);
  const clearEnded = useCalls((s) => s.clearEnded);
  const failureKind = useCalls((s) => s.failureKind);
  const retryTarget = useCalls((s) => s.retryTarget);
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const callsEnabled = useSettings((s) => s.callsEnabled);
  const [retryUnavailable, setRetryUnavailable] = useState(false);
  const canRetry =
    callsEnabled &&
    retryTarget?.owner === owner &&
    retryTarget?.generation === generation;
  const retry = () => {
    if (!retryTarget || !canRetry) return;
    const { target, video } = retryTarget;
    const deviceIds = callDeviceIds(target.accountId);
    if (!deviceIds.length) {
      setRetryUnavailable(true);
      return;
    }
    setRetryUnavailable(false);
    void useCalls.getState().startCall({ ...target, deviceIds }, video);
  };

  useCallTone(phase);

  // Auto-dismiss the terminal note (declined / busy / failed) after a moment.
  useEffect(() => {
    if (phase === "idle" && endedReason && endedReason !== "failed") {
      const id = setTimeout(() => clearEnded(), 3500);
      return () => clearTimeout(id);
    }
  }, [phase, endedReason, error, clearEnded]);

  const note = error
    ? error
    : endedReason === "decline"
      ? t("call.declined")
      : endedReason === "busy"
        ? t("call.busy")
        : t("call.failed");

  const who = rosterName || peerName || t("call.unknown");
  // The caller's name is only trustworthy when it comes from our roster (keyed by the
  // authenticated device id). For an unknown caller the name is self-asserted, so show the
  // authenticated id fingerprint alongside it so it can be cross-checked.
  const unverifiedCaller = !rosterName && phase === "incoming";
  const remoteHasVideo = (remoteStream?.getVideoTracks().length ?? 0) > 0;
  const ringing = phase === "incoming" || phase === "outgoing";
  const kindLabel = video ? t("call.video") : t("call.voice");

  return (
    <>
      <AnimatePresence>
        {phase === "idle" && endedReason && endedReason !== "failed" && (
          <motion.div
            key="call-note"
            initial={{
              opacity: 0,
              transform: motionOK
                ? "translateY(6px) translateX(-50%)"
                : "translateX(-50%)",
            }}
            animate={{
              opacity: 1,
              transform: motionOK
                ? "translateY(0px) translateX(-50%)"
                : "translateX(-50%)",
            }}
            exit={{
              opacity: 0,
              transform: motionOK
                ? "translateY(6px) translateX(-50%)"
                : "translateX(-50%)",
            }}
            transition={{ duration: motionOK ? 0.12 : 0.08, ease }}
            className="fixed bottom-4 left-1/2 z-[51]"
          >
            <div className="rounded-md border bg-card px-4 py-2 text-sm shadow-elevation">
              {note}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
      <AnimatePresence>
        {phase !== "idle" && (
          <motion.div
            key="call-surface"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, pointerEvents: "none" }}
            transition={{ duration: motionOK ? 0.16 : 0.08, ease }}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/70"
          >
            <motion.div
              initial={{
                opacity: 0,
                transform: motionOK ? "scale(.985)" : "none",
              }}
              animate={{
                opacity: 1,
                transform: motionOK ? "scale(1)" : "none",
              }}
              exit={{
                opacity: 0,
                transform: motionOK ? "scale(.985)" : "none",
              }}
              transition={{ duration: motionOK ? 0.18 : 0.08, ease }}
              className="flex w-[min(92vw,720px)] flex-col overflow-hidden rounded-xl border bg-card shadow-elevation-lg"
            >
              {/* Stage: remote video (or a glyph for ringing / audio-only), local PiP. */}
              <div className="relative aspect-video w-full bg-black">
                {/* Audio-only calls still need a player for the remote sound. */}
                {phase === "connected" && remoteStream && (
                  <StreamVideo
                    stream={remoteStream}
                    className={
                      remoteHasVideo ? "h-full w-full object-cover" : "hidden"
                    }
                  />
                )}
                {!(phase === "connected" && remoteHasVideo) && (
                  <div className="flex h-full w-full flex-col items-center justify-center gap-2 text-white">
                    {peerId && (
                      <IdentityGlyph seed={peerId} size={84} title={who} />
                    )}
                    <p className="font-display text-lg font-semibold">{who}</p>
                    {unverifiedCaller && peerId && (
                      <p className="font-mono text-xs text-white/70">
                        {shortId(peerId, 12)}
                      </p>
                    )}
                    <p className="text-sm text-white/70">
                      {t(STATUS_KEY[phase])} · {kindLabel}
                    </p>
                    {phase === "connected" && !hasVideo && (
                      <p className="text-xs text-white/50">
                        {t("call.audioOnly")}
                      </p>
                    )}
                  </div>
                )}

                {/* Local preview (muted to avoid echo). Hidden when audio-only or camera off. */}
                {localStream && hasVideo && camOn && (
                  <StreamVideo
                    stream={localStream}
                    muted
                    className="absolute bottom-3 right-3 h-28 w-40 rounded-lg border border-white/20 object-cover shadow-lg"
                  />
                )}

                {phase === "connected" && remoteHasVideo && (
                  <div className="absolute left-3 top-3 rounded-full bg-black/50 px-3 py-1 text-sm text-white">
                    {who}
                  </div>
                )}

                {/* Always mark the call as experimental. */}
                <div
                  className="absolute right-3 top-3 flex items-center gap-1 rounded-full bg-amber-500/20 px-2.5 py-1 text-xs text-amber-200"
                  title={t("call.experimentalHint")}
                >
                  <FlaskConical className="h-3 w-3" />
                  {t("call.experimental")}
                </div>
              </div>

              {/* Controls. */}
              <div className="flex items-center justify-center gap-3 p-4">
                {phase === "incoming" ? (
                  <>
                    <Button
                      onClick={() => void accept()}
                      data-testid="call-accept"
                      className="gap-2 bg-signal text-white hover:bg-signal/90"
                    >
                      <PhoneIncoming className="h-4 w-4" />
                      {t("call.accept")}
                    </Button>
                    <Button
                      onClick={() => decline()}
                      data-testid="call-decline"
                      variant="destructive"
                      className="gap-2"
                    >
                      <PhoneOff className="h-4 w-4" />
                      {t("call.decline")}
                    </Button>
                  </>
                ) : (
                  <>
                    {!ringing && (
                      <>
                        <Button
                          variant="secondary"
                          size="icon"
                          onClick={() => toggleMic()}
                          title={micOn ? t("call.mute") : t("call.unmute")}
                          data-testid="call-toggle-mic"
                        >
                          {micOn ? (
                            <Mic className="h-4 w-4" />
                          ) : (
                            <MicOff className="h-4 w-4" />
                          )}
                        </Button>
                        {hasVideo && (
                          <Button
                            variant="secondary"
                            size="icon"
                            onClick={() => toggleCam()}
                            title={
                              camOn ? t("call.cameraOff") : t("call.cameraOn")
                            }
                            data-testid="call-toggle-cam"
                          >
                            {camOn ? (
                              <VideoIcon className="h-4 w-4" />
                            ) : (
                              <VideoOff className="h-4 w-4" />
                            )}
                          </Button>
                        )}
                      </>
                    )}
                    <Button
                      onClick={() => hangup()}
                      variant="destructive"
                      className={cn("gap-2", ringing && "px-6")}
                      data-testid="call-hangup"
                    >
                      <PhoneOff className="h-4 w-4" />
                      {ringing ? t("call.cancel") : t("call.hangup")}
                    </Button>
                  </>
                )}
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
      <CallConnectionNotice
        open={phase === "idle" && endedReason === "failed"}
        onOpenChange={(open) => {
          if (!open) {
            clearEnded();
            setRetryUnavailable(false);
          }
        }}
        unavailable={retryUnavailable}
        failureKind={failureKind}
        detail={error}
        onRetry={canRetry ? retry : undefined}
      />
    </>
  );
}
