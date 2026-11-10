import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { googleDrive, type DriveProgress } from "@/lib/api/googleDrive";
export function GoogleDriveProgress({
  connectionId,
}: {
  connectionId: string;
}) {
  const { t } = useTranslation();
  const [progress, setProgress] = useState<DriveProgress | null>(null);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const next = await googleDrive.progress(connectionId);
        if (active) setProgress(next);
      } catch {
        /* Parent operation reports the actionable failure. */
      }
      if (active) timer = setTimeout(() => void poll(), 500);
    };
    void poll();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [connectionId]);
  const phase = progress?.phase;
  const label =
    phase === "uploading"
      ? t("drive.uploading")
      : phase === "downloading"
        ? t("drive.downloading")
        : t("drive.snapshot");
  return (
    <div role="status" className="space-y-2 text-xs text-muted-foreground">
      <p>{label}</p>
      {progress && progress.total > 0 && (
        <progress
          className="w-full accent-signal"
          aria-label={label}
          value={progress.transferred}
          max={progress.total}
        />
      )}
    </div>
  );
}
