import { useEffect, useRef } from "react";
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
import { presenceStatus, usePresenceFor } from "@/store/presence";
import { useCalls, type CallPhase } from "@/store/calls";
import { useChat } from "@/store/chat";
import { useSettings } from "@/store/settings";
import { shortId } from "@/lib/format";
import { playCallTone } from "@/lib/ringtones";
import { cn } from "@/lib/utils";

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
  const online = presenceStatus(presence) !== "offline";
  // Subscribe to a BOOLEAN (does the account have any discovered device?), not the whole
  // roster array — the array's identity changes every ~4s on the roster poll and would
  // re-render the header for nothing.
  const hasDevice = useChat((s) =>
    s.peers.some((p) => p.account_id === accountId),
  );
  // Opt-in: the feature is hidden entirely until enabled in Settings.
  if (!callsEnabled) return null;
  const canCall = phase === "idle" && online && hasDevice;
  const start = (video: boolean) => {
    // Read the device list fresh at click time. Ring EVERY discovered device of the account
    // (first-to-answer wins; see store/calls.ts).
    const deviceIds = useChat
      .getState()
      .peers.filter((p) => p.account_id === accountId)
      .map((p) => p.user_id);
    if (deviceIds.length > 0)
      void useCalls.getState().startCall({ name, accountId, deviceIds }, video);
  };
  return (
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
        onClick={() => start(false)}
      >
        <Phone className="h-4 w-4" />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        disabled={!canCall}
        data-testid="conversation-call-video"
        title={canCall ? t("call.video") : t("call.offlineHint")}
        onClick={() => start(true)}
      >
        <VideoIcon className="h-4 w-4" />
      </Button>
    </div>
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
    if (!ref.current) return;
    ref.current.srcObject = stream;
    // `autoPlay` alone isn't enough on autoplay-restricted webviews (WKWebView/WebKitGTK):
    // without an explicit play() the remote video/audio can stay paused though the stream
    // is flowing. Best-effort — a rejection (no user gesture yet) is harmless.
    if (stream) void ref.current.play().catch(() => {});
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
  const phase = useCalls((s) => s.phase);
  const peerName = useCalls((s) => s.peerName);
  const peerId = useCalls((s) => s.peerId);
  // Prefer the name from our own roster (keyed by the AUTHENTICATED device id) over the
  // caller's self-asserted offer name. Select the derived NAME (a stable string), not the
  // roster array, so the always-mounted overlay doesn't re-render on every ~4s roster poll.
  const rosterName = useChat(
    (s) => s.peers.find((p) => p.user_id === peerId)?.name,
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

  useCallTone(phase);

  // Auto-dismiss the terminal note (declined / busy / failed) after a moment.
  useEffect(() => {
    if (phase === "idle" && (endedReason || error)) {
      const id = setTimeout(() => clearEnded(), 3500);
      return () => clearTimeout(id);
    }
  }, [phase, endedReason, error, clearEnded]);

  if (phase === "idle") {
    if (!endedReason && !error) return null;
    const note = error
      ? error
      : endedReason === "decline"
        ? t("call.declined")
        : endedReason === "busy"
          ? t("call.busy")
          : t("call.failed");
    return (
      <div className="fixed bottom-4 left-1/2 z-50 -translate-x-1/2">
        <div className="rounded-full border bg-card px-4 py-2 text-sm shadow-lg">
          {note}
        </div>
      </div>
    );
  }

  const who = rosterName || peerName || t("call.unknown");
  // The caller's name is only trustworthy when it comes from our roster (keyed by the
  // authenticated device id). For an unknown caller the name is self-asserted, so show the
  // authenticated id fingerprint alongside it so it can be cross-checked.
  const unverifiedCaller = !rosterName && phase === "incoming";
  const remoteHasVideo = (remoteStream?.getVideoTracks().length ?? 0) > 0;
  const ringing = phase === "incoming" || phase === "outgoing";
  const kindLabel = video ? t("call.video") : t("call.voice");

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="flex w-[min(92vw,720px)] flex-col overflow-hidden rounded-2xl border bg-card shadow-2xl">
        {/* Stage: remote video (or a glyph for ringing / audio-only), local PiP. */}
        <div className="relative aspect-video w-full bg-black">
          {phase === "connected" && remoteHasVideo ? (
            <StreamVideo
              stream={remoteStream}
              className="h-full w-full object-cover"
            />
          ) : (
            <div className="flex h-full w-full flex-col items-center justify-center gap-2 text-white">
              {peerId && <IdentityGlyph seed={peerId} size={84} title={who} />}
              <p className="font-display text-lg font-semibold">{who}</p>
              {unverifiedCaller && peerId && (
                <p className="font-mono text-[11px] text-white/40">
                  {shortId(peerId, 12)}
                </p>
              )}
              <p className="text-sm text-white/70">
                {t(STATUS_KEY[phase])} · {kindLabel}
              </p>
              {phase === "connected" && !hasVideo && (
                <p className="text-xs text-white/50">{t("call.audioOnly")}</p>
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
                      title={camOn ? t("call.cameraOff") : t("call.cameraOn")}
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
      </div>
    </div>
  );
}
