import { useCallback, useEffect, useRef, useState } from "react";
import { CheckCircle2, Circle, Loader2, Video, XCircle } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { errorMessage } from "@/lib/error";

/** The risk points a single Phase-0 run validates, in order. */
type StepKey = "capture" | "devices" | "connect" | "media";
type StepStatus = "pending" | "running" | "ok" | "fail";

interface StepState {
  status: StepStatus;
  detail?: string;
}

const STEP_ORDER: StepKey[] = ["capture", "devices", "connect", "media"];

function freshSteps(): Record<StepKey, StepState> {
  return {
    capture: { status: "pending" },
    devices: { status: "pending" },
    connect: { status: "pending" },
    media: { status: "pending" },
  };
}

function StatusIcon({ status }: { status: StepStatus }) {
  if (status === "ok") return <CheckCircle2 className="h-4 w-4 text-signal" />;
  if (status === "fail")
    return <XCircle className="h-4 w-4 text-destructive" />;
  if (status === "running")
    return <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />;
  return <Circle className="h-4 w-4 text-muted-foreground/40" />;
}

/**
 * Phase-0 media self-test. Runs entirely in the webview that actually ships
 * (WKWebView / WebView2 / WebKitGTK) so the result reflects the real platform,
 * not a desktop browser. A same-page loopback proves getUserMedia + WebRTC work
 * here; cross-device reachability is a separate (network) concern noted in the UI.
 */
export function WebRtcTestDialog() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [running, setRunning] = useState(false);
  const [steps, setSteps] = useState<Record<StepKey, StepState>>(freshSteps);
  const [candidates, setCandidates] = useState<Record<string, number>>({});

  const localVideo = useRef<HTMLVideoElement>(null);
  const remoteVideo = useRef<HTMLVideoElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const pcsRef = useRef<RTCPeerConnection[]>([]);

  const setStep = useCallback((key: StepKey, next: StepState) => {
    setSteps((s) => ({ ...s, [key]: next }));
  }, []);

  const cleanup = useCallback(() => {
    for (const pc of pcsRef.current) pc.close();
    pcsRef.current = [];
    if (streamRef.current) {
      for (const tr of streamRef.current.getTracks()) tr.stop();
      streamRef.current = null;
    }
    if (localVideo.current) localVideo.current.srcObject = null;
    if (remoteVideo.current) remoteVideo.current.srcObject = null;
  }, []);

  // Release the camera/mic whenever the dialog closes.
  useEffect(() => {
    if (!open) cleanup();
  }, [open, cleanup]);
  useEffect(() => cleanup, [cleanup]);

  const run = useCallback(async () => {
    cleanup();
    setCandidates({});
    setSteps(freshSteps());
    setRunning(true);

    // 1. Capture — the permission + capture surface that historically breaks in
    //    embedded webviews. This is the make-or-break check.
    setStep("capture", { status: "running" });
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: true,
        audio: true,
      });
    } catch (e) {
      setStep("capture", { status: "fail", detail: errorMessage(e) });
      setRunning(false);
      return;
    }
    streamRef.current = stream;
    if (localVideo.current) {
      localVideo.current.srcObject = stream;
      void localVideo.current.play().catch(() => {});
    }
    const vid = stream.getVideoTracks().length;
    const aud = stream.getAudioTracks().length;
    setStep("capture", {
      status: vid > 0 && aud > 0 ? "ok" : "fail",
      detail: t("webrtcTest.captureDetail", { video: vid, audio: aud }),
    });

    // 2. Devices — how many inputs the webview can actually enumerate.
    setStep("devices", { status: "running" });
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      const cams = devices.filter((d) => d.kind === "videoinput").length;
      const mics = devices.filter((d) => d.kind === "audioinput").length;
      setStep("devices", {
        status: "ok",
        detail: t("webrtcTest.devicesDetail", { cameras: cams, mics }),
      });
    } catch (e) {
      setStep("devices", { status: "fail", detail: errorMessage(e) });
    }

    // 3 + 4. Loopback peer connection with NO ice servers — exactly the LAN
    //    setup (host candidates only). Proves RTCPeerConnection, DTLS/SRTP, and
    //    that real media frames flow end to end inside this webview.
    setStep("connect", { status: "running" });
    setStep("media", { status: "running" });
    const pc1 = new RTCPeerConnection({ iceServers: [] });
    const pc2 = new RTCPeerConnection({ iceServers: [] });
    pcsRef.current = [pc1, pc2];

    pc1.onicecandidate = (e) => {
      if (e.candidate) {
        void pc2.addIceCandidate(e.candidate);
        const type = e.candidate.type ?? "unknown";
        setCandidates((c) => ({ ...c, [type]: (c[type] ?? 0) + 1 }));
      }
    };
    pc2.onicecandidate = (e) => {
      if (e.candidate) void pc1.addIceCandidate(e.candidate);
    };

    const gotRemoteMedia = new Promise<void>((resolve) => {
      pc2.ontrack = (e) => {
        if (remoteVideo.current && e.streams[0]) {
          remoteVideo.current.srcObject = e.streams[0];
          void remoteVideo.current.play().catch(() => {});
        }
        resolve();
      };
    });

    const connected = new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(t("webrtcTest.timeout"))),
        10_000,
      );
      pc1.onconnectionstatechange = () => {
        if (pc1.connectionState === "connected") {
          clearTimeout(timer);
          resolve();
        } else if (pc1.connectionState === "failed") {
          clearTimeout(timer);
          reject(new Error("connection failed"));
        }
      };
    });

    try {
      for (const tr of stream.getTracks()) pc1.addTrack(tr, stream);
      const offer = await pc1.createOffer();
      await pc1.setLocalDescription(offer);
      await pc2.setRemoteDescription(offer);
      const answer = await pc2.createAnswer();
      await pc2.setLocalDescription(answer);
      await pc1.setRemoteDescription(answer);
      await connected;
      setStep("connect", {
        status: "ok",
        detail: t("webrtcTest.connectDetail"),
      });
      await gotRemoteMedia;
      setStep("media", { status: "ok", detail: t("webrtcTest.mediaDetail") });
    } catch (e) {
      const msg = errorMessage(e);
      setSteps((s) => ({
        connect:
          s.connect.status === "ok"
            ? s.connect
            : { status: "fail", detail: msg },
        capture: s.capture,
        devices: s.devices,
        media: { status: "fail", detail: msg },
      }));
    } finally {
      setRunning(false);
    }
  }, [cleanup, setStep, t]);

  const candidateEntries = Object.entries(candidates);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-action-webrtc-test"
          title={t("webrtcTest.trigger")}
        >
          <Video className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-2xl" data-testid="webrtc-test-dialog">
        <DialogHeader>
          <DialogTitle>{t("webrtcTest.title")}</DialogTitle>
          <DialogDescription>{t("webrtcTest.description")}</DialogDescription>
        </DialogHeader>

        <div className="max-h-[64vh] space-y-4 overflow-y-auto pr-0.5">
          <div className="flex items-center gap-3">
            <Button
              onClick={() => void run()}
              disabled={running}
              data-testid="webrtc-test-run"
            >
              {running ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Video className="h-4 w-4" />
              )}
              {running ? t("webrtcTest.running") : t("webrtcTest.run")}
            </Button>
            <span className="truncate font-mono text-[11px] text-muted-foreground">
              {navigator.userAgent}
            </span>
          </div>

          {/* The two tiles: local capture + the loopback echo coming back over WebRTC. */}
          <div className="grid grid-cols-2 gap-3">
            <VideoTile
              videoRef={localVideo}
              label={t("webrtcTest.localLabel")}
              muted
            />
            <VideoTile
              videoRef={remoteVideo}
              label={t("webrtcTest.remoteLabel")}
            />
          </div>

          <ul className="space-y-1.5">
            {STEP_ORDER.map((key) => (
              <li
                key={key}
                className="flex items-start gap-2.5 rounded-md border bg-card/40 p-2.5"
                data-testid={`webrtc-step-${key}`}
              >
                <span className="mt-0.5">
                  <StatusIcon status={steps[key].status} />
                </span>
                <div className="min-w-0">
                  <p className="text-sm font-medium">
                    {t(`webrtcTest.step.${key}`)}
                  </p>
                  {steps[key].detail && (
                    <p className="break-words text-xs text-muted-foreground">
                      {steps[key].detail}
                    </p>
                  )}
                </div>
              </li>
            ))}
          </ul>

          <section className="space-y-2 rounded-lg border bg-card/40 p-3">
            <p className="font-display text-sm font-semibold tracking-tight">
              {t("webrtcTest.candidatesTitle")}
            </p>
            <div className="flex flex-wrap gap-2">
              {candidateEntries.length === 0 ? (
                <span className="text-xs text-muted-foreground">
                  {t("webrtcTest.noCandidates")}
                </span>
              ) : (
                candidateEntries.map(([type, n]) => (
                  <Badge key={type} className="bg-muted text-muted-foreground">
                    {type} · {n}
                  </Badge>
                ))
              )}
            </div>
            <p className="text-xs leading-relaxed text-muted-foreground">
              {t("webrtcTest.candidatesHint")}
            </p>
          </section>

          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("webrtcTest.loopbackNote")}
          </p>
        </div>
      </DialogContent>
    </Dialog>
  );
}

const VideoTile = ({
  videoRef,
  label,
  muted,
}: {
  videoRef: React.RefObject<HTMLVideoElement>;
  label: string;
  muted?: boolean;
}) => (
  <div className="space-y-1">
    <p className="font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
      {label}
    </p>
    <video
      ref={videoRef}
      autoPlay
      playsInline
      muted={muted}
      className="aspect-video w-full rounded-md border bg-black object-cover"
    />
  </div>
);
