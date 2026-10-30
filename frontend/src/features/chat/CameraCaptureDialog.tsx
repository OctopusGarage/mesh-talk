import { useEffect, useRef, useState } from "react";
import { Camera, RotateCcw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";

/** One opening owns its stream and still image; only confirmation exports a File. */
export function CameraCaptureDialog({
  onCancel,
  onConfirm,
  onPickImage,
}: {
  onCancel: () => void;
  onConfirm: (file: File) => void;
  onPickImage: () => void;
}) {
  const { t } = useTranslation();
  const video = useRef<HTMLVideoElement>(null);
  const stream = useRef<MediaStream | null>(null);
  const generation = useRef(0);
  const encoding = useRef(false);
  const [attempt, setAttempt] = useState(0);
  const [deviceId, setDeviceId] = useState("");
  const [devices, setDevices] = useState<MediaDeviceInfo[]>([]);
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [mirrored, setMirrored] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [photo, setPhoto] = useState<File | null>(null);
  const [photoUrl, setPhotoUrl] = useState<string | null>(null);
  function release() {
    stream.current?.getTracks().forEach((track) => track.stop());
    stream.current = null;
    if (video.current) video.current.srcObject = null;
  }
  useEffect(() => {
    if (photo) return;
    const token = ++generation.current;
    let alive = true;
    setReady(false);
    setError(null);
    void (async () => {
      try {
        if (!navigator.mediaDevices?.getUserMedia)
          throw new DOMException("unsupported", "NotSupportedError");
        const acquired = await navigator.mediaDevices.getUserMedia({
          audio: false,
          video: {
            width: { ideal: 1920 },
            height: { ideal: 1080 },
            ...(deviceId
              ? { deviceId: { exact: deviceId } }
              : { facingMode: "user" }),
          },
        });
        if (!alive || token !== generation.current) {
          acquired.getTracks().forEach((track) => track.stop());
          return;
        }
        stream.current = acquired;
        setMirrored(
          acquired.getVideoTracks()[0]?.getSettings().facingMode === "user",
        );
        if (video.current) {
          video.current.srcObject = acquired;
          await video.current.play();
        }
        if (!alive) return;
        const inputs = await navigator.mediaDevices
          .enumerateDevices()
          .catch(() => []);
        if (alive)
          setDevices(inputs.filter((input) => input.kind === "videoinput"));
      } catch (failure) {
        if (!alive) return;
        release();
        const name =
          failure && typeof failure === "object" && "name" in failure
            ? failure.name
            : "";
        setError(
          name === "NotAllowedError" || name === "SecurityError"
            ? "denied"
            : name === "NotFoundError"
              ? "missing"
              : name === "NotSupportedError"
                ? "unsupported"
                : "unavailable",
        );
      }
    })();
    return () => {
      alive = false;
      generation.current = token + 1;
      release();
    };
  }, [attempt, deviceId, photo]);
  useEffect(() => {
    if (!photo) {
      setPhotoUrl(null);
      return;
    }
    const url = URL.createObjectURL(photo);
    setPhotoUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [photo]);
  function capture() {
    const source = video.current;
    if (
      !ready ||
      !source?.videoWidth ||
      !source.videoHeight ||
      encoding.current
    )
      return;
    encoding.current = true;
    setBusy(true);
    const token = generation.current;
    try {
      const canvas = document.createElement("canvas");
      const scale = Math.min(
        1,
        1920 / Math.max(source.videoWidth, source.videoHeight),
      );
      canvas.width = Math.max(1, Math.round(source.videoWidth * scale));
      canvas.height = Math.max(1, Math.round(source.videoHeight * scale));
      const context = canvas.getContext("2d");
      if (!context) throw new Error("canvas");
      context.drawImage(source, 0, 0, canvas.width, canvas.height);
      canvas.toBlob(
        (blob) => {
          encoding.current = false;
          if (token !== generation.current) return;
          setBusy(false);
          if (!blob) {
            setError("captureFailed");
            return;
          }
          release();
          setReady(false);
          setPhoto(
            new File([blob], `photo-${Date.now()}.jpg`, { type: "image/jpeg" }),
          );
        },
        "image/jpeg",
        0.9,
      );
    } catch {
      encoding.current = false;
      setBusy(false);
      setError("captureFailed");
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onCancel();
      }}
    >
      <DialogContent
        data-testid="camera-dialog"
        className="max-w-2xl"
        onInteractOutside={(event) => event.preventDefault()}
        onCloseAutoFocus={(event) => {
          event.preventDefault();
          document
            .querySelector<HTMLTextAreaElement>(
              '[data-testid="composer-input"]',
            )
            ?.focus();
        }}
      >
        <DialogTitle>{t("cameraCapture.title")}</DialogTitle>
        <DialogDescription>
          {t(photo ? "cameraCapture.review" : "cameraCapture.hint")}
        </DialogDescription>
        <div className="relative flex w-full aspect-[4/3] max-h-[55dvh] items-center justify-center overflow-hidden rounded-lg bg-black text-white">
          {photo && photoUrl ? (
            <img
              data-testid="camera-photo"
              src={photoUrl}
              alt={t("cameraCapture.photoAlt")}
              className="h-full w-full object-contain"
            />
          ) : (
            <video
              ref={video}
              muted
              playsInline
              autoPlay
              aria-label={t("cameraCapture.preview")}
              onLoadedData={() => setReady(true)}
              className={`h-full w-full object-contain ${mirrored ? "-scale-x-100" : ""}`}
            />
          )}
          {!photo && !ready && !error && (
            <p role="status" className="absolute px-6 text-center text-sm">
              {t("cameraCapture.starting")}
            </p>
          )}
        </div>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {t(`cameraCapture.${error}`)}
          </p>
        )}
        {!photo && devices.length > 1 && (
          <label className="flex items-center gap-3 text-sm">
            {t("cameraCapture.device")}
            <select
              aria-label={t("cameraCapture.device")}
              value={deviceId}
              disabled={busy}
              onChange={(event) => setDeviceId(event.target.value)}
              className="min-w-0 flex-1 rounded-md border bg-background p-2"
            >
              <option value="">{t("cameraCapture.defaultDevice")}</option>
              {devices.map((device, index) => (
                <option key={device.deviceId} value={device.deviceId}>
                  {device.label ||
                    t("cameraCapture.deviceNumber", { number: index + 1 })}
                </option>
              ))}
            </select>
          </label>
        )}
        <div className="flex flex-wrap items-center justify-end gap-2">
          {photo ? (
            <>
              <Button
                variant="outline"
                data-testid="camera-retake"
                onClick={() => {
                  setPhoto(null);
                  setAttempt((value) => value + 1);
                }}
              >
                <RotateCcw className="mr-2 h-4 w-4" />
                {t("cameraCapture.retake")}
              </Button>
              <Button
                data-testid="camera-use-photo"
                onClick={() => onConfirm(photo)}
              >
                {t("cameraCapture.usePhoto")}
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="outline"
                data-testid="camera-pick-image"
                onClick={onPickImage}
              >
                {t("cameraCapture.choosePhoto")}
              </Button>
              {error && (
                <Button
                  variant="outline"
                  onClick={() => setAttempt((value) => value + 1)}
                >
                  {t("cameraCapture.retry")}
                </Button>
              )}
              <Button
                data-testid="camera-shutter"
                disabled={!ready || busy || !!error}
                onClick={capture}
              >
                <Camera className="mr-2 h-4 w-4" />
                {t(busy ? "cameraCapture.capturing" : "cameraCapture.title")}
              </Button>
            </>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
