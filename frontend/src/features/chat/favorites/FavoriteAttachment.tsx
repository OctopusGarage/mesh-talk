import { useEffect, useRef, useState } from "react";
import { Download, Eye } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  messageFavorites,
  type FavoriteMessage,
} from "@/lib/api/messageFavorites";
import { captureRuntimeOwner } from "@/store/ownership";
import { saveAttachment } from "@/lib/attachmentSave";
import { humanSize } from "@/lib/format";
import { errorMessage } from "@/lib/error";
import { isVideo } from "@/lib/mediaTypes";
import { attachmentLabel } from "@/lib/directoryAttachment";
import { fileGlyph, blobMime, withinInlineCap } from "../mediaFile";

/** Favorites use their own retained-file authority, independent of cleared history. */
export function FavoriteAttachment({
  file,
}: {
  file: NonNullable<FavoriteMessage["file"]>;
}) {
  const { t } = useTranslation();
  const lease = useRef(captureRuntimeOwner()).current;
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const [ready, setReady] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    const refresh = async () => {
      if (!lease.current()) return;
      try {
        const available = await messageFavorites.fileAvailable(
          lease.owner!,
          file.file_conv,
        );
        if (active && lease.current()) setReady(available);
      } catch (e) {
        if (active && lease.current()) {
          setReady(false);
          setError(errorMessage(e));
        }
      }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 15_000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [file.file_conv, lease]);
  useEffect(
    () => () => {
      if (url) URL.revokeObjectURL(url);
    },
    [url],
  );
  const save = async () => {
    if (busy || !ready || !lease.current()) return;
    setBusy(true);
    setError(null);
    try {
      const path = await saveAttachment(
        { ...file, fileConv: file.file_conv },
        lease,
        { mode: "as", source: "favorite" },
      );
      if (lease.current() && path) setSaved(true);
    } catch (e) {
      if (lease.current()) setError(errorMessage(e));
    } finally {
      if (lease.current()) setBusy(false);
    }
  };
  const preview = async () => {
    if (
      busy ||
      !ready ||
      !lease.current() ||
      !withinInlineCap(file.name, file.size)
    )
      return;
    setBusy(true);
    setError(null);
    try {
      const bytes = await messageFavorites.readFile(
        lease.owner!,
        file.file_conv,
      );
      if (alive.current && lease.current())
        setUrl(
          URL.createObjectURL(
            new Blob([bytes], { type: blobMime(file.name, file.mime) }),
          ),
        );
    } catch (e) {
      if (lease.current()) setError(errorMessage(e));
    } finally {
      if (lease.current()) setBusy(false);
    }
  };
  return (
    <div className="space-y-2 rounded-md border p-3">
      <div className="flex items-start gap-2">
        {fileGlyph(file.name, file.mime)}
        <div className="min-w-0 flex-1">
          <p className="break-words text-sm font-medium">
            {attachmentLabel(file.name, file.mime)}
          </p>
          <p className="text-xs text-muted-foreground">
            {humanSize(file.size)} ·{" "}
            {t(
              ready === null
                ? "common.loading"
                : ready
                  ? "personal.saved"
                  : "messageFavorites.fileUnavailable",
            )}
          </p>
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          variant="outline"
          disabled={!ready || busy}
          onClick={() => void save()}
        >
          <Download className="mr-1 h-4 w-4" aria-hidden="true" />
          {t("common.save")}
        </Button>
        {withinInlineCap(file.name, file.size) && !url && (
          <Button
            size="sm"
            variant="ghost"
            disabled={!ready || busy}
            onClick={() => void preview()}
          >
            <Eye className="mr-1 h-4 w-4" aria-hidden="true" />
            {t("messageFavorites.preview")}
          </Button>
        )}
      </div>
      {url &&
        (isVideo(file.name) ? (
          <video
            src={url}
            controls
            preload="metadata"
            className="max-h-80 max-w-full"
            aria-label={file.name}
          />
        ) : (
          <img
            src={url}
            alt={file.name}
            className="max-h-80 max-w-full object-contain"
          />
        ))}
      {saved && (
        <p role="status" className="text-xs text-muted-foreground">
          {t("personal.saved")}
        </p>
      )}
      {error && (
        <p role="alert" className="break-words text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
