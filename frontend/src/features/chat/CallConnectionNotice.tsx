import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { CallFailureKind } from "@/store/calls";
import { DiagnosticsDialog } from "./DiagnosticsDialog";

/** Explain current capability without treating a discovered route as a media guarantee. */
export function CallConnectionNotice({
  open,
  onOpenChange,
  unavailable = false,
  failureKind,
  detail,
  onRetry,
  returnFocus,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  unavailable?: boolean;
  failureKind?: CallFailureKind | null;
  detail?: string | null;
  onRetry?: () => void;
  returnFocus?: () => void;
}) {
  const { t } = useTranslation();
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent
          onCloseAutoFocus={(event) => {
            if (returnFocus && !diagnosticsOpen) {
              event.preventDefault();
              returnFocus();
            }
          }}
        >
          <DialogHeader>
            <DialogTitle>
              {t(unavailable ? "call.unavailableTitle" : "call.failed")}
            </DialogTitle>
            <DialogDescription>
              {t(
                unavailable
                  ? "call.unavailableHint"
                  : failureKind === "capture"
                    ? "call.captureHint"
                    : failureKind === "timeout"
                      ? "call.timeoutHint"
                      : "call.connectionHint",
              )}
            </DialogDescription>
          </DialogHeader>
          {detail && (
            <details className="min-w-0 text-sm text-muted-foreground">
              <summary className="cursor-pointer">
                {t("call.technicalDetails")}
              </summary>
              <p className="mt-2 max-h-32 overflow-auto whitespace-pre-wrap break-words">
                {detail}
              </p>
            </details>
          )}
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.close")}
            </Button>
            <Button
              variant="outline"
              data-testid="call-check-connection"
              onClick={() => {
                onOpenChange(false);
                setDiagnosticsOpen(true);
              }}
            >
              {t("call.checkConnection")}
            </Button>
            {onRetry && (
              <Button data-testid="call-retry" onClick={onRetry}>
                {t("call.retry")}
              </Button>
            )}
          </div>
        </DialogContent>
      </Dialog>
      {diagnosticsOpen && (
        <DiagnosticsDialog
          open
          onOpenChange={setDiagnosticsOpen}
          initialTab="help"
          onCloseAutoFocus={(event) => {
            if (returnFocus) {
              event.preventDefault();
              returnFocus();
            }
          }}
        />
      )}
    </>
  );
}
