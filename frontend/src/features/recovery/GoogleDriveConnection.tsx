import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { googleDrive } from "@/lib/api/googleDrive";
import { errorMessage } from "@/lib/error";

/** Each mounted view owns one ephemeral native grant, including pending OAuth. */
export function GoogleDriveConnection({
  connectionId,
  onConnected,
  disabled = false,
}: {
  connectionId: string;
  onConnected: () => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const [available, setAvailable] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState("");
  const lifetime = useRef(0);
  const pending = useRef(false);
  useEffect(() => {
    const run = ++lifetime.current;
    void googleDrive.available().then(
      (value) => {
        if (run === lifetime.current) setAvailable(value);
      },
      (cause) => {
        if (run === lifetime.current) setError(errorMessage(cause));
      },
    );
    return () => {
      lifetime.current = run + 1;
      void googleDrive.disconnect(connectionId).catch(() => {});
    };
  }, [connectionId]);
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
        onConnected();
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
      <p className="text-xs text-muted-foreground">{t("drive.privacy")}</p>
      {available === false ? (
        <p role="status">{t("drive.unavailable")}</p>
      ) : (
        !connected && (
          <Button
            variant="secondary"
            size="sm"
            disabled={disabled || busy || available !== true}
            onClick={() => void connect()}
          >
            {busy ? t("drive.connecting") : t("drive.connect")}
          </Button>
        )
      )}
      {busy && (
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            lifetime.current++;
            void googleDrive
              .disconnect(connectionId)
              .catch((cause) => setError(errorMessage(cause)))
              .finally(() => {
                pending.current = false;
                setBusy(false);
              });
          }}
        >
          {t("common.cancel")}
        </Button>
      )}
      {connected && (
        <p role="status" className="text-xs">
          {t("drive.connected")}
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
