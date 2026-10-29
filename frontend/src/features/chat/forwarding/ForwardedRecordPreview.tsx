import { useEffect, useState } from "react";
import { FileText, ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { save } from "@tauri-apps/plugin-dialog";
import { messageForwarding } from "@/lib/api/messageForwarding";
import { chat } from "@/lib/api/chat";
import {
  decodeForwardedRecord,
  FORWARDED_RECORD_MAX_BYTES,
  type ForwardedFile,
  type ForwardedRecord,
} from "@/lib/forwardedRecord";
import { humanSize } from "@/lib/format";
import { captureChatOwnership, useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import { useFileStatus } from "@/store/fileAvailability";

/** Object URLs belong to this mounted transcript, never to the source conversation. */
function RecordAttachment({
  file,
  fileConv,
  index,
}: {
  file: ForwardedFile;
  fileConv: string;
  index: number;
}) {
  const { t } = useTranslation();
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState(false);
  const saveFile = async () => {
    const lease = captureChatOwnership();
    if (!lease.owner || !lease.current() || saving) return;
    setSaving(true);
    setSaveError(false);
    try {
      const destination = await save({ defaultPath: file.name });
      if (typeof destination !== "string" || !lease.current()) return;
      await messageForwarding.exportAttachment(
        lease.owner,
        fileConv,
        index,
        destination,
      );
    } catch {
      if (lease.current()) setSaveError(true);
    } finally {
      setSaving(false);
    }
  };
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    const value = URL.createObjectURL(
      new Blob([new Uint8Array(file.bytes)], { type: file.mime }),
    );
    setUrl(value);
    return () => URL.revokeObjectURL(value);
  }, [file]);
  const image = /^(image\/(png|jpeg|gif|webp))$/.test(file.mime);
  const video = /^(video\/(mp4|webm|quicktime))$/.test(file.mime);
  const audio = /^(audio\/(mpeg|mp4|ogg|wav|webm))$/.test(file.mime);
  return (
    <div className="mt-2 min-w-0">
      {url && image && (
        <img
          src={url}
          alt={file.name}
          loading="lazy"
          className="max-h-96 max-w-full rounded-md object-contain"
        />
      )}
      {url && video && (
        <video
          src={url}
          controls
          preload="metadata"
          className="max-h-96 max-w-full rounded-md"
        />
      )}
      {url && audio && (
        <audio src={url} controls preload="metadata" className="max-w-full" />
      )}
      <button
        type="button"
        onClick={() => void saveFile()}
        disabled={saving}
        className="mt-2 rounded-md border px-3 py-2 text-xs font-medium hover:bg-muted disabled:opacity-50"
      >
        {t("messageForwarding.recordSave")}
      </button>
      {saveError && (
        <p role="alert" className="mt-1 text-xs text-destructive">
          {t("messageForwarding.recordSaveFailed")}
        </p>
      )}
      <p className="mt-1 break-words text-xs text-muted-foreground">
        {t("messageForwarding.recordAttachment", { name: file.name })} ·{" "}
        {humanSize(file.size)}
      </p>
    </div>
  );
}

/** The enclosing file bubble retains consent/save/reveal; this adds a typed read-only view. */
export function ForwardedRecordPreview({
  fileConv,
  name,
  size,
}: {
  fileConv: string;
  name: string;
  size: number;
  mime?: string;
}) {
  const { t } = useTranslation();
  const status = useFileStatus(fileConv);
  const authScope = useAuth((state) => `${state.user?.id}:${state.generation}`);
  const runtimeScope = useChat(
    (state) => `${state.runEpoch}:${state.identityEpoch}:${state.ready}`,
  );
  const scope = `${authScope}:${runtimeScope}`;
  const [open, setOpen] = useState(false);
  const [loaded, setLoaded] = useState<{
    scope: string;
    fileConv: string;
    record: ForwardedRecord;
  } | null>(null);
  const [error, setError] = useState(false);
  const record =
    loaded?.scope === scope && loaded.fileConv === fileConv
      ? loaded.record
      : null;
  const ready = !!status?.ready && size <= FORWARDED_RECORD_MAX_BYTES;
  useEffect(() => {
    if (!open || !ready) return;
    let active = true;
    const lease = captureChatOwnership();
    setError(false);
    setLoaded(null);
    void (async () => {
      let bytes: ArrayBuffer;
      try {
        bytes = await chat.readMedia(fileConv);
      } catch {
        if (!active || !lease.current()) return;
        bytes = await chat.readFile(fileConv);
      }
      if (!active || !lease.current()) return;
      const parsed = decodeForwardedRecord(bytes);
      setLoaded({ scope, fileConv, record: parsed });
    })().catch(() => {
      if (active && lease.current()) setError(true);
    });
    return () => {
      active = false;
    };
  }, [open, ready, fileConv, scope]);
  return (
    <Dialog open={open && ready} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <button
          type="button"
          disabled={!ready}
          data-testid="forwarded-record-open"
          className="mt-2 flex w-full min-w-0 items-center gap-3 rounded-lg border bg-muted/40 p-3 text-left hover:bg-muted disabled:opacity-60"
          title={!ready ? t("messageForwarding.recordUnavailable") : undefined}
        >
          <FileText className="h-5 w-5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1">
            <span className="block text-sm font-medium">
              {t("messageForwarding.openRecord")}
            </span>
            <span className="block truncate text-xs text-muted-foreground">
              {name}
            </span>
          </span>
          <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground" />
        </button>
      </DialogTrigger>
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] w-[calc(100vw-2rem)] min-w-0 max-w-2xl overflow-x-hidden"
        data-testid="forwarded-record-viewer"
      >
        <DialogHeader>
          <DialogTitle className="break-words pr-8">
            {record?.title || t("messageForwarding.recordTitle")}
          </DialogTitle>
          <DialogDescription>
            {t("messageForwarding.recordDescription")}
          </DialogDescription>
        </DialogHeader>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {t("messageForwarding.recordInvalid")}
          </p>
        ) : !record ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("messageForwarding.recordLoading")}
          </p>
        ) : (
          <>
            <p className="text-xs text-muted-foreground">
              {t("messageForwarding.recordCount", {
                count: record.messages.length,
              })}
            </p>
            <ol className="divide-y" data-testid="forwarded-record-messages">
              {record.messages.map((message, index) => (
                <li key={index} className="min-w-0 py-4 first:pt-0">
                  <div className="mb-1 flex flex-wrap items-baseline gap-x-3 gap-y-1">
                    <span className="break-words text-sm font-medium">
                      {message.sender_name}
                    </span>
                    <time
                      dateTime={new Date(message.wall_clock).toISOString()}
                      className="text-xs text-muted-foreground"
                    >
                      {new Date(message.wall_clock).toLocaleString()}
                    </time>
                  </div>
                  {message.text && (
                    <p className="whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">
                      {message.text}
                    </p>
                  )}
                  {message.file && (
                    <RecordAttachment
                      file={message.file}
                      fileConv={fileConv}
                      index={index}
                    />
                  )}
                </li>
              ))}
            </ol>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
