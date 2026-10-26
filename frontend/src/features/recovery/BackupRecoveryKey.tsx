import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";

/** One-time recovery setup. Acknowledgement never depends on retyping the key. */
export function BackupRecoveryKey({
  recoveryKey,
  confirmed,
  disabled,
}: {
  recoveryKey: string;
  confirmed: boolean;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const visible = !confirmed || revealed;
  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <p className="font-medium">
          {t(confirmed ? "recovery.keyReady" : "recovery.saveKeyOnce")}
        </p>
        <p className="text-xs leading-relaxed text-muted-foreground">
          {t(confirmed ? "recovery.keyReadyHint" : "recovery.keySetupHint")}
        </p>
      </div>
      {visible && (
        <>
          <code
            data-testid="recovery-export-key"
            className="block select-all break-words rounded-md border bg-muted/35 p-3 font-mono text-xs leading-6"
          >
            {recoveryKey}
          </code>
          <Button
            variant="outline"
            size="sm"
            disabled={disabled}
            onClick={() => {
              setCopyError(false);
              void (async () => {
                try {
                  await navigator.clipboard.writeText(recoveryKey);
                  setCopied(true);
                } catch {
                  setCopyError(true);
                }
              })();
            }}
          >
            {t(copied ? "recovery.keyCopied" : "recovery.copyKey")}
          </Button>
          {copyError && (
            <p role="alert" className="text-xs text-destructive">
              {t("recovery.keyCopyFailed")}
            </p>
          )}
        </>
      )}
      {confirmed && (
        <Button
          variant="ghost"
          size="sm"
          disabled={disabled}
          onClick={() => setRevealed(!revealed)}
        >
          {t(revealed ? "recovery.hideKey" : "recovery.viewKey")}
        </Button>
      )}
    </div>
  );
}
