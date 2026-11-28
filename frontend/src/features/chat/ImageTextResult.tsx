import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { chat } from "@/lib/api";
import { captureChatOwnership } from "@/store/chat";
import { copySensitiveText } from "@/lib/sensitiveClipboard";
import { Button } from "@/components/ui/button";

/** Plaintext exists only while this source-scoped preview is mounted. */
export function ImageTextResult({
  png,
  onClose,
}: {
  png: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [lease] = useState(captureChatOwnership);
  const [text, setText] = useState<string>();
  const [failed, setFailed] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    setText(undefined);
    setFailed(false);
    if (!lease.owner || !lease.current()) return;
    void chat
      .recognizeImageText(lease.owner, png)
      .then((value) => {
        if (active && lease.current()) setText(value);
      })
      .catch(() => {
        if (active && lease.current()) setFailed(true);
      });
    return () => {
      active = false;
    };
  }, [png, attempt, lease]);
  return (
    <section
      aria-label={t("files.extractText")}
      onClick={(event) => event.stopPropagation()}
      className="absolute inset-x-6 bottom-24 top-16 z-10 flex flex-col rounded-xl border border-border bg-background p-4 text-foreground shadow-sm sm:left-auto sm:w-96"
    >
      <div className="mb-3 flex items-center justify-between gap-3">
        <h2 className="font-medium">{t("files.extractText")}</h2>
        <Button size="sm" variant="ghost" onClick={onClose}>
          {t("common.close")}
        </Button>
      </div>
      <div role="status" className="text-sm text-muted-foreground">
        {failed
          ? t("files.textRecognitionFailed")
          : text === undefined
            ? t("files.recognizingText")
            : !text
              ? t("files.noTextFound")
              : null}
      </div>
      {text && (
        <div
          tabIndex={0}
          className="selectable min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words text-sm leading-relaxed"
          onCopy={(event) => {
            const selected = window.getSelection()?.toString();
            if (!selected) return;
            event.preventDefault();
            event.stopPropagation();
            if (lease.current())
              void copySensitiveText(
                selected,
                t("settings.confirmCopyPrompt"),
                (value) => event.clipboardData.setData("text/plain", value),
              ).catch(() => {
                if (lease.current()) setCopyFailed(true);
              });
          }}
        >
          {text}
        </div>
      )}
      {copyFailed && (
        <p role="alert" className="mt-2 text-sm text-destructive">
          {t("common.copyFailed")}
        </p>
      )}
      <div className="mt-auto flex justify-end gap-2 pt-4">
        {failed && (
          <Button
            variant="outline"
            onClick={() => setAttempt((value) => value + 1)}
          >
            {t("common.retry")}
          </Button>
        )}
        {text && (
          <Button
            data-copy-text
            onClick={() => {
              if (!lease.current()) return;
              setCopyFailed(false);
              void copySensitiveText(
                text,
                t("settings.confirmCopyPrompt"),
              ).catch(() => {
                if (lease.current()) setCopyFailed(true);
              });
            }}
          >
            {t("files.copyAllText")}
          </Button>
        )}
      </div>
    </section>
  );
}
