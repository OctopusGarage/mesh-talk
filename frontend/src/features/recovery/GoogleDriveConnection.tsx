import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { googleDrive } from "@/lib/api/googleDrive";
import { errorMessage } from "@/lib/error";

/** View connections retire on unmount; remembered native grants survive closing. */
export function GoogleDriveConnection({
  connectionId,
  onConnected,
  onDisconnected,
  disabled = false,
}: {
  connectionId: string;
  onConnected: () => void;
  onDisconnected?: () => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const [attempt, setAttempt] = useState(0);
  const [available, setAvailable] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [resuming, setResuming] = useState(true);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState("");
  const lifetime = useRef(0);
  const pending = useRef(false);
  const callbacks = useRef({ onConnected, onDisconnected });
  useEffect(() => {
    callbacks.current = { onConnected, onDisconnected };
  });
  useEffect(() => {
    const run = ++lifetime.current;
    setConnected(false);
    setResuming(true);
    setAvailable(null);
    setError("");
    pending.current = true;
    void (async () => {
      try {
        const value = await googleDrive.available();
        if (run !== lifetime.current) return;
        setAvailable(value);
        if (value && (await googleDrive.resume(connectionId))) {
          if (run !== lifetime.current) return;
          setConnected(true);
          callbacks.current.onConnected();
        }
      } catch (cause) {
        if (run === lifetime.current) setError(errorMessage(cause));
      } finally {
        if (run === lifetime.current) {
          pending.current = false;
          setResuming(false);
        }
      }
    })();
    return () => {
      lifetime.current = lifetime.current + 1;
      void googleDrive.disconnect(connectionId).catch(() => {});
    };
  }, [connectionId, attempt]);
  const connect = async () => {
    if (pending.current) return;
    pending.current = true;
    const run = lifetime.current;
    setBusy(true);
    setError("");
    try {
      await googleDrive.connect(connectionId);
      if (run === lifetime.current) {
        setConnected(true);
        callbacks.current.onConnected();
      }
    } catch (cause) {
      if (run === lifetime.current) setError(errorMessage(cause));
    } finally {
      if (run === lifetime.current) {
        pending.current = false;
        setBusy(false);
      }
    }
  };
  return (
    <div className="space-y-2">
      {available === false ? (
        <p role="status" className="text-xs text-muted-foreground">
          {t("drive.unavailable")}
        </p>
      ) : resuming ? (
        <p role="status" className="text-xs text-muted-foreground">
          {t("drive.checkingConnection")}
        </p>
      ) : connected ? (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p role="status" className="text-xs">
            {t("drive.connected")}
          </p>
          <Button
            variant="ghost"
            size="sm"
            disabled={disabled || busy}
            onClick={() => {
              if (pending.current) return;
              pending.current = true;
              const run = lifetime.current;
              setBusy(true);
              setError("");
              void googleDrive
                .forget(connectionId)
                .then(
                  () => {
                    if (run === lifetime.current) {
                      setConnected(false);
                      callbacks.current.onDisconnected?.();
                    }
                  },
                  (cause) => {
                    if (run === lifetime.current) setError(errorMessage(cause));
                  },
                )
                .finally(() => {
                  if (run === lifetime.current) {
                    pending.current = false;
                    setBusy(false);
                  }
                });
            }}
          >
            {t("drive.reconnect")}
          </Button>
        </div>
      ) : (
        <Button
          variant="secondary"
          size="sm"
          disabled={disabled || busy || available !== true}
          onClick={() => void connect()}
        >
          {busy ? t("drive.connecting") : t("drive.connect")}
        </Button>
      )}
      {!resuming && !connected && error && (
        <Button
          variant="outline"
          size="sm"
          disabled={disabled || busy}
          onClick={() => setAttempt((value) => value + 1)}
        >
          {t("contactVisibility.retry")}
        </Button>
      )}
      {busy && !connected && (
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            const run = ++lifetime.current;
            void googleDrive
              .disconnect(connectionId)
              .catch((cause) => {
                if (run === lifetime.current) setError(errorMessage(cause));
              })
              .finally(() => {
                if (run === lifetime.current) {
                  pending.current = false;
                  setBusy(false);
                }
              });
          }}
        >
          {t("common.cancel")}
        </Button>
      )}
      {error && (
        <p role="alert" className="break-words text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
