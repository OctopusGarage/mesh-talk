import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";
import { recordVoiceClip } from "./attachments/voiceRecording";

export function VoiceCaptureDialog({
  onCancel,
  onSend,
}: {
  onCancel: () => void;
  onSend: (file: File) => Promise<boolean>;
}) {
  const { t } = useTranslation();
  const active = useRef<AbortController | null>(null);
  const [stop, setStop] = useState<(() => void) | null>(null);
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState(false);
  const [clip, setClip] = useState<File | null>(null);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => () => active.current?.abort(), []);
  useEffect(() => {
    if (!clip) {
      setPreviewUrl(null);
      return;
    }
    const url = URL.createObjectURL(clip);
    setPreviewUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [clip]);

  async function record() {
    if (active.current || sending) return;
    const controller = new AbortController();
    active.current = controller;
    setClip(null);
    setError(false);
    setBusy(true);
    try {
      const file = await recordVoiceClip(controller.signal, (finish) =>
        setStop(() => () => {
          setStop(null);
          finish();
        }),
      );
      if (controller.signal.aborted) return;
      if (file) setClip(file);
      else setError(true);
    } catch {
      if (!controller.signal.aborted) setError(true);
    } finally {
      if (!controller.signal.aborted) {
        active.current = null;
        setStop(null);
        setBusy(false);
      }
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !sending) onCancel();
      }}
    >
      <DialogContent className="max-w-sm" data-testid="voice-capture-dialog">
        <DialogTitle>{t("voiceCapture.title")}</DialogTitle>
        <DialogDescription>{t("voiceCapture.description")}</DialogDescription>
        {clip && previewUrl && (
          <audio className="w-full" controls src={previewUrl} />
        )}
        {error && <p role="alert">{t("voiceCapture.failed")}</p>}
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="outline" disabled={sending} onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button
            variant={clip ? "outline" : "default"}
            disabled={(busy && !stop) || sending}
            onClick={() => {
              if (stop) stop();
              else void record();
            }}
          >
            {stop
              ? t("voiceCapture.stop")
              : busy
                ? t("common.loading")
                : clip
                  ? t("voiceCapture.again")
                  : t("voiceCapture.record")}
          </Button>
          {clip && (
            <Button
              disabled={sending}
              onClick={() => {
                setSending(true);
                void onSend(clip).then((accepted) => {
                  if (!accepted) {
                    setSending(false);
                    setError(true);
                  }
                });
              }}
            >
              {t("voiceCapture.send")}
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
