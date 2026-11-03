import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { chat } from "@/lib/api";
import type { ChannelMetadata } from "@/lib/types";
import { pickImageFile } from "@/lib/avatarImage";
import { captureRuntimeOwner } from "@/store/ownership";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { AvatarCropDialog } from "./AvatarCropDialog";
import { useAnnouncementAuthor } from "./useGroupAnnouncement";

const ANNOUNCEMENT_LIMIT = 4096;
const COVER_LIMIT = 32 * 1024;

/** An editor lifetime belongs to one group, account, session and authority snapshot. */
export function ChannelMetadataSettings(props: {
  channelId: string;
  canManage: boolean;
}) {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const account = useChat((s) => s.myAccountId);
  return (
    <MetadataEditor
      key={`${props.channelId}:${owner}:${generation}:${account}:${props.canManage}`}
      {...props}
    />
  );
}

function MetadataEditor({
  channelId,
  canManage,
}: {
  channelId: string;
  canManage: boolean;
}) {
  const { t } = useTranslation();
  const fieldId = useId();
  const account = useChat((s) => s.myAccountId);
  const [metadata, setMetadata] = useState<ChannelMetadata | null>(null);
  const publisherName = useAnnouncementAuthor(metadata?.announcement?.author);
  const coverRevision = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata?.cover
        ?.revision,
  );
  const announcementRevision = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata?.announcement
        ?.revision,
  );
  const readRequest = useRef(0);
  const readBusy = useRef(false);
  const readAgain = useRef(false);
  const pendingState = useChat((s) => {
    const current = s.channels.find(
      (c) => c.channel_id === channelId,
    )?.metadata;
    return `${!!current?.query_pending}:${!!current?.cover_pending}:${!!current?.announcement_pending}`;
  });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [preview, setPreview] = useState(false);
  const [draft, setDraft] = useState("");
  const [baseRevision, setBaseRevision] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleteRevision, setDeleteRevision] = useState<string | null>(null);
  const [crop, setCrop] = useState<{
    file: File;
    revision: string | null;
  } | null>(null);
  const [lifetime] = useState(() => ({ live: true }));
  useEffect(() => {
    lifetime.live = true;
    return () => {
      lifetime.live = false;
    };
  }, [lifetime]);
  const bytes = new TextEncoder().encode(draft.trim()).length;
  const refresh = useCallback(
    async function refreshMetadata(): Promise<void> {
      const lease = captureRuntimeOwner();
      if (!lease.owner || !lease.current() || !account) return;
      const request = ++readRequest.current;
      if (readBusy.current) {
        readAgain.current = true;
        return;
      }
      readBusy.current = true;
      setError(null);
      try {
        const result = await chat.channelMetadata(
          lease.owner,
          account,
          channelId,
        );
        if (
          lifetime.live &&
          lease.current() &&
          readRequest.current === request
        ) {
          setMetadata(result);
        }
      } catch {
        if (lifetime.live && lease.current() && readRequest.current === request)
          setError(t("groupMetadata.loadFailed"));
      } finally {
        readBusy.current = false;
        if (readAgain.current && lifetime.live) {
          readAgain.current = false;
          void refreshMetadata();
        }
      }
    },
    [account, channelId, lifetime, t],
  );
  useEffect(() => {
    void refresh();
  }, [refresh, coverRevision, announcementRevision, pendingState]);
  useEffect(() => {
    if (!metadata?.query_pending) return;
    const timer = window.setTimeout(() => void refresh(), 250);
    return () => window.clearTimeout(timer);
  }, [metadata, refresh]);
  const save = async (
    kind: "cover" | "announcement",
    content: string | null,
    revision: string | null,
  ) => {
    const lease = captureRuntimeOwner();
    if (
      !canManage ||
      !lifetime.live ||
      busy ||
      !metadata ||
      metadata.query_pending ||
      !lease.owner ||
      !lease.current() ||
      !account
    )
      return;
    if (
      kind === "cover" ? metadata.cover_pending : metadata.announcement_pending
    )
      return;
    if (
      content !== null &&
      (kind === "cover"
        ? content.length > COVER_LIMIT
        : !content.trim() ||
          new TextEncoder().encode(content).length > ANNOUNCEMENT_LIMIT)
    ) {
      setError(
        t(
          kind === "cover"
            ? "groupMetadata.coverTooLarge"
            : "groupMetadata.tooLong",
        ),
      );
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const command =
        kind === "cover" ? chat.setChannelCover : chat.setChannelAnnouncement;
      const result = await command(
        lease.owner,
        account,
        channelId,
        content,
        revision,
      );
      if (!lifetime.live || !lease.current()) return;
      readRequest.current++;
      setMetadata(result);
      if (kind === "announcement") {
        setEditing(false);
        setPreview(false);
        setConfirmDelete(false);
      }
      void useChat.getState().refreshChannels(channelId);
    } catch {
      if (lifetime.live && lease.current())
        setError(t("groupMetadata.saveFailed"));
    } finally {
      if (lifetime.live && lease.current()) setBusy(false);
    }
  };
  const chooseCover = async () => {
    const lease = captureRuntimeOwner();
    const revision = metadata?.cover?.revision ?? null;
    try {
      const file = await pickImageFile();
      if (file && lifetime.live && lease.current()) setCrop({ file, revision });
    } catch {
      if (lifetime.live && lease.current())
        setError(t("groupMetadata.loadFailed"));
    }
  };
  return (
    <section
      className="space-y-4 border-t border-border pt-4"
      aria-label={t("groupMetadata.settings")}
    >
      <div className="space-y-2">
        <h3 className="text-sm font-medium">{t("groupMetadata.cover")}</h3>
        <p className="text-xs text-muted-foreground">
          {t("groupMetadata.coverHelp")}
        </p>
        {metadata?.cover?.content && (
          <img
            src={metadata.cover.content}
            alt={t("groupMetadata.cover")}
            className="h-16 w-16 rounded-lg object-cover"
          />
        )}
        {metadata?.cover_pending && (
          <p role="status" className="text-xs text-muted-foreground">
            {t("groupMetadata.waiting")}
          </p>
        )}
        {canManage && (
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={
                !metadata ||
                busy ||
                metadata.query_pending ||
                metadata.cover_pending
              }
              onClick={() => void chooseCover()}
            >
              {t(
                metadata?.cover?.content
                  ? "groupMetadata.changeCover"
                  : "groupMetadata.setCover",
              )}
            </Button>
            {metadata?.cover?.content && (
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={() =>
                  void save("cover", null, metadata.cover!.revision)
                }
              >
                {t("groupMetadata.removeCover")}
              </Button>
            )}
          </div>
        )}
      </div>
      <div className="space-y-2 border-t border-border pt-4">
        <h3 className="text-sm font-medium">
          {t("groupMetadata.announcement")}
        </h3>
        {(!metadata || metadata.query_pending) && !error && (
          <p role="status" className="text-sm text-muted-foreground">
            {t("common.loading")}
          </p>
        )}
        {!editing && (
          <>
            {metadata && !metadata.query_pending && (
              <p
                data-testid="group-announcement-content"
                className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-relaxed [overflow-wrap:anywhere]"
              >
                {metadata.announcement_pending
                  ? t("groupMetadata.waiting")
                  : (metadata.announcement?.content ??
                    t("groupMetadata.empty"))}
              </p>
            )}
            {metadata?.announcement?.content && (
              <p className="break-all text-xs text-muted-foreground">
                {t("groupMetadata.publishedBy", {
                  author: publisherName,
                  time: new Date(
                    metadata.announcement.authored_at,
                  ).toLocaleString(),
                })}
              </p>
            )}
            {canManage && (
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={
                    !metadata ||
                    busy ||
                    metadata.query_pending ||
                    metadata.announcement_pending
                  }
                  onClick={() => {
                    setDraft(metadata?.announcement?.content ?? "");
                    setBaseRevision(metadata?.announcement?.revision ?? null);
                    setEditing(true);
                    setPreview(false);
                    setError(null);
                  }}
                >
                  {t(
                    metadata?.announcement?.content
                      ? "groupMetadata.edit"
                      : "groupMetadata.write",
                  )}
                </Button>
                {metadata?.announcement?.content && (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => {
                      setDeleteRevision(metadata.announcement!.revision);
                      setConfirmDelete(true);
                    }}
                  >
                    {t("groupMetadata.delete")}
                  </Button>
                )}
              </div>
            )}
          </>
        )}
        {editing && (
          <>
            <label htmlFor={fieldId} className="sr-only">
              {t("groupMetadata.announcement")}
            </label>
            {preview ? (
              <div
                data-testid="announcement-preview"
                className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-relaxed [overflow-wrap:anywhere]"
              >
                {draft.trim()}
              </div>
            ) : (
              <textarea
                id={fieldId}
                value={draft}
                disabled={busy}
                onChange={(e) => setDraft(e.target.value)}
                aria-describedby={`${fieldId}-limit`}
                className="min-h-32 w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
            )}
            <p
              id={`${fieldId}-limit`}
              className="text-xs text-muted-foreground"
            >
              {t("groupMetadata.limit", {
                count: bytes,
                limit: ANNOUNCEMENT_LIMIT,
              })}
            </p>
            <p className="text-xs text-muted-foreground">
              {t("groupMetadata.publishHelp")}
            </p>
            <div className="flex flex-wrap gap-2">
              {preview ? (
                <>
                  <Button
                    size="sm"
                    disabled={busy || !bytes || bytes > ANNOUNCEMENT_LIMIT}
                    onClick={() =>
                      void save("announcement", draft.trim(), baseRevision)
                    }
                  >
                    {t(
                      busy
                        ? "groupMetadata.publishing"
                        : "groupMetadata.publish",
                    )}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => setPreview(false)}
                  >
                    {t("groupMetadata.backToEdit")}
                  </Button>
                </>
              ) : (
                <Button
                  size="sm"
                  disabled={busy || !bytes || bytes > ANNOUNCEMENT_LIMIT}
                  onClick={() => setPreview(true)}
                >
                  {t("groupMetadata.preview")}
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={() => {
                  setEditing(false);
                  setError(null);
                }}
              >
                {t("common.cancel")}
              </Button>
            </div>
          </>
        )}
        {confirmDelete && (
          <div
            className="space-y-2"
            role="group"
            aria-label={t("groupMetadata.delete")}
          >
            <p className="text-sm">{t("groupMetadata.deleteConfirm")}</p>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="destructive"
                size="sm"
                disabled={busy}
                onClick={() => void save("announcement", null, deleteRevision)}
              >
                {t("groupMetadata.confirmDelete")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={() => setConfirmDelete(false)}
              >
                {t("common.cancel")}
              </Button>
            </div>
          </div>
        )}
      </div>
      {error && (
        <div className="space-y-2">
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => {
              setEditing(false);
              setPreview(false);
              setConfirmDelete(false);
              void refresh();
            }}
          >
            {t("groupMetadata.refresh")}
          </Button>
        </div>
      )}
      <AvatarCropDialog
        file={crop?.file ?? null}
        onCancel={() => setCrop(null)}
        onConfirm={(content) => {
          const revision = crop?.revision ?? null;
          setCrop(null);
          void save("cover", content, revision);
        }}
      />
    </section>
  );
}
