import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useAppLock } from "@/store/appLock";
import { useCalls } from "@/store/calls";
import { captureRuntimeOwner } from "@/store/ownership";
import {
  openMediaDeviceCheck,
  MediaDeviceCheckError,
  type MediaDeviceCheckSession,
  type MediaDeviceKind,
} from "@/lib/mediaDeviceCheck";

export function MediaDeviceCheck() {
  const { t } = useTranslation();
  const owner = useAuth((s) => s.user?.id),
    generation = useAuth((s) => s.generation);
  const run = useChat((s) => s.runEpoch),
    identity = useChat((s) => s.identityEpoch);
  const locked = useAppLock((s) => s.locked),
    phase = useCalls((s) => s.phase);
  const busyCall = ["outgoing", "incoming", "connecting", "connected"].includes(
    phase,
  );
  const [kind, setKind] = useState<MediaDeviceKind | null>(null);
  const [status, setStatus] = useState(""),
    [error, setError] = useState<string | null>(null);
  const [level, setLevel] = useState(0),
    [diagnostic, setDiagnostic] = useState("");
  const [checking, setChecking] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const video = useRef<HTMLVideoElement>(null);
  const session = useRef<MediaDeviceCheckSession | null>(null);
  const abort = useRef<AbortController | null>(null);
  const disposeMeter = useRef<(() => void) | null>(null);
  const cleanup = useCallback(() => {
    abort.current?.abort();
    abort.current = null;
    session.current?.stop();
    session.current = null;
    disposeMeter.current?.();
    disposeMeter.current = null;
    if (video.current) {
      video.current.pause();
      video.current.srcObject = null;
    }
  }, []);
  useEffect(() => {
    cleanup();
    setKind(null);
    setStatus("");
    setError(null);
    setLevel(0);
    setDiagnostic("");
    setChecking(false);
    return cleanup;
  }, [owner, generation, run, identity, locked, busyCall, cleanup]);
  const start = async (next: MediaDeviceKind) => {
    const lease = captureRuntimeOwner();
    if (!owner || locked || busyCall || !lease.current()) return;
    cleanup();
    const controller = new AbortController();
    abort.current = controller;
    const current = () =>
      abort.current === controller &&
      !controller.signal.aborted &&
      lease.current();
    setKind(next);
    setStatus("capturing");
    setError(null);
    setLevel(0);
    setDiagnostic("");
    setChecking(false);
    try {
      const result = await openMediaDeviceCheck(next, controller.signal);
      if (!current()) {
        result.stop();
        return;
      }
      session.current = result;
      setStatus(next === "audio" ? "audioMeterUnavailable" : "videoReady");
      if (next === "audio" && typeof AudioContext !== "undefined") {
        try {
          const context = new AudioContext();
          const nodes: AudioNode[] = [];
          let frame = 0;
          let disposed = false;
          // Register the owner before any analysis operation can throw.
          const dispose = () => {
            if (disposed) return;
            disposed = true;
            cancelAnimationFrame(frame);
            for (const node of nodes) {
              try {
                node.disconnect();
              } catch {
                /* A failed setup may leave an unconnected node. */
              }
            }
            try {
              void context.close().catch(() => {});
            } catch {
              /* Already closed by the platform. */
            }
          };
          disposeMeter.current = dispose;
          const source = context.createMediaStreamSource(result.stream);
          nodes.push(source);
          const analyser = context.createAnalyser();
          nodes.push(analyser);
          analyser.fftSize = 256;
          source.connect(analyser);
          const values = new Uint8Array(analyser.fftSize);
          const sample = () => {
            if (!current() || disposed) return;
            try {
              analyser.getByteTimeDomainData(values);
              const energy = Math.sqrt(
                values.reduce(
                  (sum, value) => sum + ((value - 128) / 128) ** 2,
                  0,
                ) / values.length,
              );
              setLevel(Math.min(100, Math.round(energy * 400)));
              frame = requestAnimationFrame(sample);
            } catch {
              dispose();
              setStatus("audioMeterUnavailable");
            }
          };
          void context
            .resume()
            .then(() => {
              if (current() && !disposed && context.state === "running")
                setStatus("audioReady");
            })
            .catch(() => {
              dispose();
            });
          sample();
        } catch {
          disposeMeter.current?.();
          disposeMeter.current = null;
          // Meter support is optional; successful microphone capture remains usable.
          setStatus("audioMeterUnavailable");
        }
      }
    } catch (failure) {
      if (!current()) return;
      cleanup();
      setKind(null);
      setStatus("");
      setError(
        failure instanceof MediaDeviceCheckError ? failure.code : "failed",
      );
    }
  };
  useEffect(() => {
    const element = video.current;
    if (
      !element ||
      kind !== "video" ||
      status !== "videoReady" ||
      !session.current
    )
      return;
    element.srcObject = session.current.stream;
    void element.play().catch(() => {});
    return () => {
      element.pause();
      element.srcObject = null;
    };
  }, [kind, status]);
  const loopback = async () => {
    const active = session.current,
      controller = abort.current;
    const lease = captureRuntimeOwner();
    if (!active || !controller || checking || !lease.current()) return;
    setChecking(true);
    setDiagnostic("checkingLoopback");
    setError(null);
    try {
      await active.runLoopback();
      if (
        abort.current === controller &&
        !controller.signal.aborted &&
        lease.current()
      )
        setDiagnostic("loopbackPassed");
    } catch (failure) {
      if (
        abort.current !== controller ||
        controller.signal.aborted ||
        !lease.current()
      )
        return;
      cleanup();
      setKind(null);
      setStatus("");
      setDiagnostic("");
      setChecking(false);
      setError(
        failure instanceof MediaDeviceCheckError ? failure.code : "failed",
      );
    } finally {
      if (abort.current === controller) setChecking(false);
    }
  };
  return (
    <section
      id="setting-media-device-check"
      tabIndex={-1}
      aria-labelledby="media-device-check-title"
      className="space-y-3 border-t py-4"
      data-testid="media-device-check"
    >
      <div>
        <h3 id="media-device-check-title" className="text-sm font-medium">
          {t("mediaCheck.title")}
        </h3>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
          {t("mediaCheck.description")}
        </p>
      </div>
      {busyCall && (
        <p className="text-xs text-muted-foreground">
          {t("mediaCheck.activeCall")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {(["audio", "video"] as const).map((device) => (
          <Button
            key={device}
            type="button"
            size="sm"
            variant="outline"
            disabled={!owner || locked || busyCall || kind === device}
            data-testid={`media-check-${device}`}
            onClick={() => void start(device)}
          >
            {t(`mediaCheck.${device}`)}
          </Button>
        ))}
        {kind && (
          <Button
            type="button"
            size="sm"
            variant="ghost"
            data-testid="media-check-stop"
            onClick={() => {
              cleanup();
              setKind(null);
              setStatus("stopped");
              setLevel(0);
              setDiagnostic("");
              setChecking(false);
            }}
          >
            {t("mediaCheck.stop")}
          </Button>
        )}
      </div>
      <p
        role="status"
        data-testid="media-check-status"
        className="text-xs text-muted-foreground"
      >
        {status ? t(`mediaCheck.${status}`) : ""}
      </p>
      {error && (
        <p
          role="alert"
          data-testid="media-check-error"
          className="text-xs text-destructive"
        >
          {t(`mediaCheck.errors.${error}`)}
        </p>
      )}
      {kind === "audio" && status === "audioReady" && (
        <meter
          min={0}
          max={100}
          value={level}
          aria-label={t("mediaCheck.level")}
          data-testid="media-check-level"
          className="h-3 w-full accent-[hsl(var(--signal))]"
        />
      )}
      {kind === "video" && (
        <video
          ref={video}
          muted
          autoPlay
          playsInline
          aria-label={t("mediaCheck.cameraPreview")}
          data-testid="media-check-camera-preview"
          className="max-h-64 w-full rounded-md bg-black object-contain"
        />
      )}
      <details
        open={advanced}
        onToggle={(event) => setAdvanced(event.currentTarget.open)}
        data-testid="media-check-advanced"
        className="text-xs"
      >
        <summary className="cursor-pointer py-2 font-medium">
          {t("mediaCheck.advanced")}
        </summary>
        <p className="mb-3 leading-relaxed text-muted-foreground">
          {t("mediaCheck.advancedHint")}
        </p>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={!session.current || checking || status === "capturing"}
          data-testid="media-check-loopback"
          onClick={() => void loopback()}
        >
          {t("mediaCheck.runLoopback")}
        </Button>
        <p
          role="status"
          data-testid="media-check-diagnostic-status"
          className="mt-2 text-muted-foreground"
        >
          {diagnostic ? t(`mediaCheck.${diagnostic}`) : ""}
        </p>
      </details>
    </section>
  );
}
