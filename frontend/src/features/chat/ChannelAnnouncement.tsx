import {
  useNewGroupAnnouncement,
  useAnnouncementAuthor,
} from "./useGroupAnnouncement";
import { useState } from "react";
import { Megaphone, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import type { ChannelMetadataValue } from "@/lib/types";
import { dismissAnnouncement } from "@/lib/groupAnnouncements";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";

export function AnnouncementDetails({
  channelId,
  open,
  onOpenChange,
}: {
  channelId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const pending = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata
        ?.announcement_pending,
  );
  const value = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata
        ?.announcement,
  );
  const publisherName = useAnnouncementAuthor(value?.author);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("groupMetadata.announcement")}</DialogTitle>
          <DialogDescription>
            {value?.content
              ? t("groupMetadata.publishedBy", {
                  author: publisherName,
                  time: new Date(value.authored_at).toLocaleString(),
                })
              : t(pending ? "groupMetadata.waiting" : "groupMetadata.empty")}
          </DialogDescription>
        </DialogHeader>
        <p className="whitespace-pre-wrap break-words text-sm leading-relaxed [overflow-wrap:anywhere]">
          {value?.content ??
            t(pending ? "groupMetadata.waiting" : "groupMetadata.empty")}
        </p>
      </DialogContent>
    </Dialog>
  );
}

export function ChannelAnnouncementStrip({ channelId }: { channelId: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const owner = useAuth((s) => s.user?.id ?? "");
  const account = useChat((s) => s.myAccountId);
  const value = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata
        ?.announcement,
  );
  const unseen = useNewGroupAnnouncement(channelId);
  const acknowledge = () => {
    if (value) dismissAnnouncement(owner, account, channelId, value.revision);
  };
  return (
    <>
      {unseen && (
        <div
          data-testid="group-announcement-reminder"
          className="flex min-w-0 items-center gap-2 border-b border-border bg-card px-4 py-1 text-sm"
        >
          <Megaphone aria-hidden className="h-4 w-4 shrink-0 text-signal" />
          <button
            type="button"
            className="flex min-h-10 min-w-0 flex-1 items-center gap-2 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => {
              acknowledge();
              setOpen(true);
            }}
          >
            <span className="shrink-0 font-medium">
              {t("groupMetadata.newAnnouncement")}
            </span>
            <span className="truncate text-muted-foreground">
              {value?.content}
            </span>
          </button>
          <button
            type="button"
            aria-label={t("groupMetadata.dismiss")}
            onClick={acknowledge}
            className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}
      <AnnouncementDetails
        channelId={channelId}
        open={open}
        onOpenChange={setOpen}
      />
    </>
  );
}

/** One card per accepted event; opening always resolves the current announcement. */
export function AnnouncementPublication({
  channelId,
  value,
}: {
  channelId: string;
  value: ChannelMetadataValue;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const owner = useAuth((s) => s.user?.id ?? "");
  const account = useChat((s) => s.myAccountId);
  const current = useChat(
    (s) =>
      s.channels.find((c) => c.channel_id === channelId)?.metadata
        ?.announcement,
  );
  return (
    <div
      data-testid="announcement-publication"
      className="mx-auto my-3 max-w-md px-4"
    >
      <button
        type="button"
        onClick={() => {
          if (current)
            dismissAnnouncement(owner, account, channelId, current.revision);
          setOpen(true);
        }}
        className="w-full space-y-2 rounded-lg border border-border bg-card p-3 text-left text-sm outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="flex items-center gap-2 font-medium">
          <Megaphone aria-hidden className="h-4 w-4 text-signal" />
          {t("groupMetadata.announcement")}
        </span>
        <span className="block whitespace-pre-wrap break-words text-muted-foreground [overflow-wrap:anywhere]">
          {value.content === null
            ? t("groupMetadata.removed")
            : current?.revision === value.revision
              ? value.content
              : t("groupMetadata.earlier")}
        </span>
        <span className="block text-xs text-muted-foreground">
          {new Date(value.authored_at).toLocaleString()} ·{" "}
          {t("groupMetadata.viewCurrent")}
        </span>
      </button>
      <AnnouncementDetails
        channelId={channelId}
        open={open}
        onOpenChange={setOpen}
      />
    </div>
  );
}
