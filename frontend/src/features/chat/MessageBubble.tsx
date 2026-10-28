import { canCollect } from "./favorites/selection";
import { ProtectedMessage } from "./ProtectedMessage";
import {
  retryAutomaticDownload,
  useAutomaticDownloads,
} from "@/lib/automaticDownloads";
import { createAttachmentDecision } from "./attachments/attachmentDecision";
import { useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Bookmark,
  ListChecks,
  Copy,
  CornerUpLeft,
  Download,
  FolderOpen,
  RotateCw,
  SmilePlus,
  Trash2,
  Undo2,
} from "lucide-react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { IdentityGlyph } from "@/components/identity";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { cn } from "@/lib/utils";
import { chat } from "@/lib/api";
import { saveAttachment } from "@/lib/attachmentSave";
import {
  attachmentLabel,
  isDirectoryAttachment,
} from "@/lib/directoryAttachment";
import { useSavedDownloads } from "@/lib/savedDownloads";
import { errorMessage } from "@/lib/error";
import { copySensitiveText } from "@/lib/sensitiveClipboard";
import { formatTime, humanSize, shortId } from "@/lib/format";
import { fadeSlideUp } from "@/lib/motion";
import { EMOJIS, renderWithMentions } from "@/lib/mentions";
import { stickerById } from "@/lib/stickerPacks";
import { usePacks } from "@/store/packs";
import type { ReactionInfo } from "@/lib/types";
import {
  useChat,
  captureChatOwnership,
  reactionFromMe,
  type ChatMessage,
} from "@/store/chat";
import {
  useFileStatus,
  refreshFileAvailability,
  markUnavailableFileError,
} from "@/store/fileAvailability";
import { stickerSummary } from "@/lib/stickerMedia";
import { messageSticker } from "@/lib/messagePresentation";
import { MediaSticker } from "./MediaSticker";
import { ReplyPreview } from "./ReplyPreview";
import { MediaPreview } from "./MediaPreview";
import { DeliveryFooter } from "./DeliveryFooter";
import { fileGlyph, withinInlineCap } from "./mediaFile";
import { ReceiveProgress } from "./ReceiveProgress";
import { TransferBar } from "./TransferBar";

/** A file/media message body: inline media for images/small videos, else a file card with
 * a Save action. Reuses the shared MediaPreview (lazy bytes + revoked blob URLs). */
function FileBubble({
  file,
  mine,
  metadataPending,
  failed,
  sendKey,
  messageId,
  isChannel,
  isPersonal,
  readOnly,
}: {
  file: NonNullable<ChatMessage["file"]>;
  mine: boolean;
  metadataPending?: boolean;
  failed?: boolean;
  sendKey?: string;
  messageId?: string | null;
  isChannel: boolean;
  isPersonal: boolean;
  readOnly: boolean;
}) {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const savedPath = useSavedDownloads()[file.fileConv];
  const status = useFileStatus(
    savedPath ? undefined : file.fileConv || undefined,
  );
  const available = !!savedPath || !!status?.ready;
  const [saving, setSaving] = useState(false);
  const autoFailure = useAutomaticDownloads((s) => s.failures[file.fileConv]);
  const automatic = useChat(
    (s) =>
      !mine &&
      s.incomingFiles.some((entry) => entry.fileConv === file.fileConv),
  );
  const directory = isDirectoryAttachment(file.mime);
  const approvalNeeded =
    !mine && (status?.consent === "pending" || status?.consent === "declined");
  const uploadKey =
    mine && !metadataPending && !isChannel && !isPersonal
      ? messageId || undefined
      : undefined;

  const saveAs = async () => {
    const lease = captureChatOwnership();
    if (
      !lease.current() ||
      metadataPending ||
      !file.fileConv ||
      !available ||
      saving
    )
      return;
    try {
      setSaving(true);
      await saveAttachment(file, lease, {
        mode: directory ? "default" : "as",
      });
    } catch (e) {
      if (lease.current() && markUnavailableFileError(file.fileConv, e)) return;
      if (lease.current())
        setError(t("files.couldntSave", { error: errorMessage(e) }));
    } finally {
      setSaving(false);
    }
  };

  const reveal = async () => {
    if (!savedPath) return;
    const lease = captureChatOwnership();
    try {
      await revealItemInDir(savedPath);
    } catch (e) {
      if (lease.current())
        setError(t("files.couldntOpen", { error: errorMessage(e) }));
    }
  };

  const decideAttachment = createAttachmentDecision({
    captureOwnership: captureChatOwnership,
    decideAttachment: chat.decideAttachment,
    refreshAvailability: refreshFileAvailability,
    reportError: (error) =>
      setError(t("files.approvalFailed", { error: errorMessage(error) })),
  });
  const decide = (accept: boolean) => decideAttachment(file.fileConv, accept);

  // Media (image/video, sent via the media button) that is small enough to render inline:
  // show ONLY the preview. Its filename, size, and download live in the detail view that
  // opens on click (MediaPreview's lightbox) — the chat doesn't need that clutter under
  // every photo. Media that is too large to preview inline falls through to the file card
  // below, so it stays visible and savable instead of rendering an empty bubble.
  if (
    !metadataPending &&
    !approvalNeeded &&
    file.fileConv &&
    file.media &&
    withinInlineCap(file.name, file.size)
  ) {
    return (
      <div data-inline-media="" className="max-w-xs">
        <MediaPreview
          fileConv={file.fileConv}
          name={file.name}
          size={file.size}
          mime={file.mime}
          readOnly={readOnly}
        />
        {uploadKey && (
          <TransferBar
            transferKey={uploadKey}
            waitingDirection={sendKey ? "send" : undefined}
          />
        )}
      </div>
    );
  }

  // Attachment (or over-cap media): the file card IS the content — name + size + download.
  return (
    <div className="min-w-[12rem] max-w-xs">
      <div className="flex items-center gap-2">
        {fileGlyph(file.name, file.mime)}
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm">
            {attachmentLabel(file.name, file.mime, file, t("message.sticker"))}
          </div>
          <div className="font-mono text-[11px] tabular-nums opacity-70">
            {metadataPending
              ? t(
                  failed
                    ? "message.fail.unknown"
                    : "message.delivery.metadataPending",
                )
              : readOnly && !available
                ? t("recovery.mediaUnavailable")
                : humanSize(file.size)}
          </div>
        </div>
        <button
          type="button"
          onClick={() => {
            if (savedPath) void reveal();
            else if (automatic) retryAutomaticDownload(file.fileConv);
            else void saveAs();
          }}
          disabled={
            metadataPending ||
            !file.fileConv ||
            (!savedPath &&
              (!available || saving || (automatic && !autoFailure)))
          }
          title={
            metadataPending
              ? t("transfer.sending")
              : !available
                ? t(readOnly ? "recovery.mediaUnavailable" : "transfer.waiting")
                : savedPath
                  ? t("files.savedTo", { path: savedPath })
                  : t(
                      automatic
                        ? autoFailure
                          ? "contactVisibility.retry"
                          : "common.loading"
                        : "common.save",
                    )
          }
          aria-label={
            savedPath
              ? t("files.reveal")
              : t(
                  automatic
                    ? autoFailure
                      ? "contactVisibility.retry"
                      : "common.loading"
                    : "common.save",
                )
          }
          data-download-error={autoFailure || undefined}
          className={cn(
            "rounded-md p-1 transition-colors disabled:cursor-not-allowed disabled:opacity-40",
            mine
              ? "text-primary-foreground/80 hover:bg-primary-foreground/15"
              : "text-muted-foreground hover:bg-accent hover:text-foreground",
          )}
        >
          {savedPath ? (
            <FolderOpen className="h-4 w-4" />
          ) : (
            <Download className="h-4 w-4" />
          )}
        </button>
      </div>
      {metadataPending && mine && !failed && (
        <TransferBar transferKey={sendKey} waitingDirection="prepare" />
      )}
      {uploadKey && (
        <TransferBar
          transferKey={uploadKey}
          waitingDirection={sendKey ? "send" : undefined}
        />
      )}
      {autoFailure && (
        <div role="alert" className="mt-1 text-xs text-destructive">
          {t("files.couldntSave", { error: autoFailure })}
        </div>
      )}
      {!readOnly && !mine && !metadataPending && !savedPath && (
        <ReceiveProgress status={status} />
      )}
      {!readOnly && approvalNeeded && !savedPath && (
        <div className="mt-2 flex gap-2">
          <button
            data-testid="receive-file"
            type="button"
            onClick={() => void decide(true)}
            className="rounded-md bg-primary px-2.5 py-1 text-xs text-primary-foreground"
            aria-label={t("files.approve")}
          >
            {t("files.approve")}
          </button>
          {status?.consent === "pending" && (
            <button
              type="button"
              onClick={() => void decide(false)}
              className="rounded-md border px-2.5 py-1 text-xs"
              aria-label={t("files.decline")}
            >
              {t("files.decline")}
            </button>
          )}
        </div>
      )}
      {savedPath && (
        <div className="mt-1 truncate text-[11px] opacity-70" title={savedPath}>
          {t("files.savedTo", { path: savedPath })}
        </div>
      )}
    </div>
  );
}

export function MessageBubble({
  m,
  parent,
  onNavigateToParent,
  parentAuthorName,
  parentNavigationPending = false,
  showAuthor,
  grouped,
  showTime,
  isChannel,
  isPersonal = false,
  readOnly = false,
  authorAvatarId,
  authorName,
  onViewAuthor,
  fresh,
  mentioned,
  reactions,
  selfReactionId,
  myName: _myName,
  onReply,
  onReact,
  onRetry,
  onDelete,
  onRecall,
  onReEdit,
  onCollect,
  onSelectMessages,
  collected = false,
}: {
  onCollect?: (m: ChatMessage) => void;
  onSelectMessages?: (m: ChatMessage) => void;
  collected?: boolean;
  m: ChatMessage;
  parent: ChatMessage | null;
  onNavigateToParent?: (id: string) => void;
  parentAuthorName?: string;
  parentNavigationPending?: boolean;
  showAuthor: boolean;
  /** Adjacent message from the same person within a short interval. */
  grouped?: boolean;
  /** The final message in a group carries the visible time. */
  showTime?: boolean;
  /** Channel conversations show per-message author avatar + name; DMs don't. */
  isChannel: boolean;
  isPersonal?: boolean;
  /** Recovered channel history has no sender keys on this device. */
  readOnly?: boolean;
  /** The author's ACCOUNT id (resolved from the roster) for avatar lookup; defaults to
   *  the device id (m.who) when unknown. Only used for the channel author glyph. */
  authorAvatarId?: string;
  /** The author's display NAME (resolved from the roster/members); falls back to the raw
   *  user-id when unknown. Only used for the channel author label. */
  authorName?: string;
  /** Opens the author's member card from a channel avatar or name. */
  onViewAuthor?: () => void;
  /** True only for a genuinely newly-arriving/sent message → plays an entrance. */
  fresh?: boolean;
  /** True when this channel message mentions the current user. */
  mentioned?: boolean;
  reactions: ReactionInfo[];
  // Compatibility fallback when the native channel ownership projection is absent:
  // account ID for account conversations; device ID for channels.
  selfReactionId: string;
  myName: string;
  onReply: (m: ChatMessage) => void;
  onReact: (target: string, emoji: string) => void;
  onRetry: (clientId: string) => void;
  onDelete: (m: ChatMessage) => void;
  onRecall: (m: ChatMessage) => void;
  onReEdit: (text: string) => void;
}) {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const installedPacks = usePacks((s) => s.packs);
  const displayedText = m.protection ? t("protectedMessage.label") : m.text;
  const mine = m.fromMe;
  const isFile = !!m.file;
  // Sticker messages render bubble-less (Telegram-style): just the animation. Fall back to
  // the big emoji char when this build doesn't bundle that sticker (version skew).
  const presentation = messageSticker(m, installedPacks);
  const mediaSticker = presentation?.media ? presentation : null;
  const stickerDescription = presentation
    ? stickerSummary(presentation, t("message.sticker"))
    : t("message.sticker");
  const sticker =
    !mediaSticker && m.sticker
      ? stickerById(m.sticker, installedPacks)
      : undefined;
  const isSticker = !!presentation;
  const [pickerOpen, setPickerOpen] = useState(false);
  // Copy the message text to the clipboard. File messages have no text → no-op.
  const copyText = async () => {
    const lease = captureChatOwnership();
    if (!m.text || m.protection) return;
    try {
      await copySensitiveText(displayedText, t("settings.confirmCopyPrompt"));
    } catch (e) {
      if (lease.current()) setError(errorMessage(e));
    }
  };
  // Prefer the author's display name (resolved from the roster/members); fall back to the
  // raw `who` user-id (rendered mono when it's a long identifier) only if no name is known.
  const isIdName = !authorName && m.who.length > 20;
  const authorLabel = authorName || (isIdName ? shortId(m.who, 10) : m.who);

  // Accessible summary of the bubble: who, the text, and the time — folding the transient
  // pending/failed state so a screen reader announces the message's status too.
  const speaker = mine ? t("common.you") : authorLabel;
  const status = m.pending
    ? t("message.sending")
    : m.failed
      ? t(`message.fail.${m.failReason ?? "unknown"}`)
      : "";
  const body = m.protection
    ? displayedText
    : isSticker
      ? stickerDescription
      : isFile
        ? t("files.sentFile", { name: m.file!.name })
        : displayedText;
  const ariaLabel = `${speaker}: ${body} · ${formatTime(m.wallClock)}${
    status ? ` · ${status}` : ""
  }`;

  // Recall is only offered for my own, still-sendable messages within the 2-minute window
  // (matches the core's check; the command re-validates authoritatively). `menuNow` is
  // refreshed when the context menu opens, so the item disappears once the window has
  // elapsed instead of lingering from a stale render-time snapshot and erroring on click.
  const RECALL_WINDOW_MS = 2 * 60 * 1000;
  const [menuNow, setMenuNow] = useState(() => Date.now());
  const canRecall =
    !readOnly &&
    mine &&
    !!m.id &&
    !m.pending &&
    menuNow - m.wallClock < RECALL_WINDOW_MS;

  // A recalled message is a centered system line ("X recalled a message") — no bubble,
  // actions or reactions. Deleting it locally is still offered via the context menu.
  if (m.recalled) {
    return (
      <motion.div
        role="article"
        data-testid="message-recalled"
        aria-label={
          mine
            ? t("message.recalledByYou")
            : t("message.recalledBy", { name: authorLabel })
        }
        initial={fresh ? "hidden" : false}
        animate="visible"
        variants={fadeSlideUp}
        className="flex items-center justify-center gap-2 px-4 py-1"
      >
        <ContextMenu>
          <ContextMenuTrigger asChild>
            <span
              data-context-menu=""
              className="rounded-full bg-muted/60 px-3 py-1 text-xs italic text-muted-foreground"
            >
              {mine
                ? t("message.recalledByYou")
                : t("message.recalledBy", { name: authorLabel })}
            </span>
          </ContextMenuTrigger>
          <ContextMenuContent data-testid="message-context-menu">
            <ContextMenuItem
              data-testid="msg-delete"
              disabled={!m.id}
              onSelect={() => onDelete(m)}
            >
              <Trash2 className="h-3.5 w-3.5 opacity-70" />
              {t("message.deleteLocalAction")}
            </ContextMenuItem>
          </ContextMenuContent>
        </ContextMenu>
        {/* Re-edit: only for our own recalled TEXT messages (WeChat). Drops the original
            text back into the composer. */}
        {mine && m.recalledText && !m.protection && !readOnly && (
          <button
            type="button"
            data-testid="msg-reedit"
            onClick={() => onReEdit(m.recalledText!)}
            className="text-xs font-medium text-signal underline-offset-2 hover:underline"
          >
            {t("message.reEdit")}
          </button>
        )}
      </motion.div>
    );
  }

  return (
    <motion.div
      role="article"
      data-testid="message-bubble"
      aria-label={ariaLabel}
      initial={fresh ? "hidden" : false}
      animate="visible"
      variants={fadeSlideUp}
      className={cn(
        "group flex gap-2.5 px-4",
        grouped ? "py-0.5" : "pt-2.5 pb-0.5",
        mine && "flex-row-reverse",
      )}
    >
      {/* Avatar gutter only in channels; DMs align flush with no empty gutter. */}
      {isChannel && (
        <div className="w-8 shrink-0">
          {showAuthor &&
            !mine &&
            (onViewAuthor ? (
              <button
                type="button"
                onClick={onViewAuthor}
                aria-label={t("memberProfile.viewNamed", { name: authorLabel })}
                className="rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <IdentityGlyph
                  seed={authorAvatarId ?? m.who}
                  size={32}
                  title={authorLabel}
                />
              </button>
            ) : (
              <IdentityGlyph
                seed={authorAvatarId ?? m.who}
                size={32}
                title={authorLabel}
              />
            ))}
        </div>
      )}

      <div
        className={cn(
          // min-w-0 lets this column shrink below its content's intrinsic width so
          // max-w-[72%] is respected and long/preformatted lines wrap instead of
          // forcing the bubble to full width (and overflowing off-screen).
          "flex min-w-0 max-w-[72%] flex-col",
          mine ? "items-end" : "items-start",
        )}
      >
        {showAuthor &&
          !mine &&
          (onViewAuthor ? (
            <button
              type="button"
              onClick={onViewAuthor}
              className={cn(
                "mb-1 rounded px-1 text-left text-xs font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
                isIdName ? "font-mono" : "font-display tracking-tight",
              )}
            >
              {authorLabel}
            </button>
          ) : (
            <span
              className={cn(
                "mb-1 px-1 text-xs font-medium text-muted-foreground",
                isIdName ? "font-mono" : "font-display tracking-tight",
              )}
            >
              {authorLabel}
            </span>
          ))}

        {mentioned && !mine && (
          <span className="mb-1 rounded-full bg-signal/10 px-2 py-0.5 text-[11px] font-medium text-signal">
            {t("message.mentionedYou")}
          </span>
        )}

        <div className="flex min-w-0 items-center gap-1">
          {/* hover actions (left of bubble for own messages) */}
          {mine && (
            <Actions
              onReply={() => onReply(m)}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onPick={(e) => m.id && onReact(m.id, e)}
              disabled={!m.id || readOnly}
            />
          )}

          <ContextMenu
            onOpenChange={(open) => {
              if (open) setMenuNow(Date.now());
            }}
          >
            <ContextMenuTrigger asChild>
              <div
                // Marks this region so the prod global contextmenu-disable lets our
                // custom menu open here (see main.tsx).
                data-context-menu=""
                className={cn(
                  "min-w-0 text-[14px] leading-[1.55]",
                  // Stickers float; actual inline media removes its text-bubble shell.
                  // File cards and consent prompts retain the ordinary bubble.
                  isSticker
                    ? ""
                    : cn(
                        "rounded-xl px-3.5 py-2.5 [&:has([data-inline-media])]:border-0 [&:has([data-inline-media])]:bg-transparent [&:has([data-inline-media])]:p-0",
                        mine
                          ? "rounded-br-sm bg-bubble-own text-[hsl(var(--bubble-own-foreground))]"
                          : "rounded-bl-sm border border-border bg-muted text-foreground",
                      ),
                  mentioned && "ring-1 ring-inset ring-signal/65",
                  m.pending && "opacity-75",
                )}
              >
                {m.protection ? (
                  <ProtectedMessage
                    id={m.id}
                    protection={m.protection}
                    image={!!m.file}
                    mime={m.file?.mime}
                  />
                ) : mediaSticker ? (
                  <MediaSticker
                    key={m.file!.fileConv || m.clientId}
                    fileConv={m.file!.fileConv}
                    mime={m.file!.mime}
                    fallback={mediaSticker.fallback}
                    label={presentation?.label}
                    readOnly={readOnly}
                  />
                ) : isSticker ? (
                  sticker ? (
                    <img
                      src={sticker.url}
                      alt={stickerDescription}
                      title={presentation?.label}
                      data-testid="message-sticker"
                      draggable={false}
                      className="h-32 w-32 max-w-full select-none object-contain"
                    />
                  ) : (
                    // Version skew: peer sent a sticker this build doesn't bundle → show
                    // the fallback emoji char big.
                    <span
                      data-testid="message-sticker-fallback"
                      role="img"
                      aria-label={stickerDescription}
                      className="text-6xl"
                    >
                      {displayedText}
                    </span>
                  )
                ) : isFile ? (
                  <FileBubble
                    file={m.file!}
                    mine={mine}
                    metadataPending={m.metadataPending}
                    failed={m.failed}
                    sendKey={m.clientId}
                    messageId={m.id}
                    isChannel={isChannel}
                    isPersonal={isPersonal}
                    readOnly={readOnly}
                  />
                ) : (
                  <span className="cursor-text select-text whitespace-pre-wrap [overflow-wrap:anywhere]">
                    {renderWithMentions(displayedText, { ownBubble: mine })}
                  </span>
                )}
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent data-testid="message-context-menu">
              {onCollect && canCollect(m) && (
                <ContextMenuItem
                  data-testid="msg-collect"
                  onSelect={() => onCollect(m)}
                >
                  <Bookmark className="h-3.5 w-3.5 opacity-70" />
                  {t(
                    collected
                      ? "messageFavorites.saved"
                      : "messageFavorites.add",
                  )}
                </ContextMenuItem>
              )}
              {onSelectMessages && canCollect(m) && (
                <ContextMenuItem
                  data-testid="msg-select-messages"
                  onSelect={() => onSelectMessages(m)}
                >
                  <ListChecks className="h-3.5 w-3.5 opacity-70" />
                  {t("messageFavorites.selectMessages")}
                </ContextMenuItem>
              )}
              <ContextMenuItem
                data-testid="msg-copy"
                disabled={!m.text || !!m.protection}
                onSelect={() => void copyText()}
              >
                <Copy className="h-3.5 w-3.5 opacity-70" />
                {t("common.copy")}
              </ContextMenuItem>
              <ContextMenuItem
                data-testid="msg-reply"
                disabled={!m.id || readOnly}
                onSelect={() => onReply(m)}
              >
                <CornerUpLeft className="h-3.5 w-3.5 opacity-70" />
                {t("message.reply")}
              </ContextMenuItem>
              <ContextMenuItem
                data-testid="msg-react"
                disabled={!m.id || readOnly}
                // Defer so the menu finishes closing before the picker popover opens
                // (avoids a focus/layer fight between the two radix overlays).
                onSelect={() => setTimeout(() => setPickerOpen(true), 0)}
              >
                <SmilePlus className="h-3.5 w-3.5 opacity-70" />
                {t("message.react")}
              </ContextMenuItem>
              {canRecall && (
                <ContextMenuItem
                  data-testid="msg-recall"
                  onSelect={() => onRecall(m)}
                >
                  <Undo2 className="h-3.5 w-3.5 opacity-70" />
                  {t("message.recall")}
                </ContextMenuItem>
              )}
              <ContextMenuItem
                data-testid="msg-delete"
                className="text-destructive focus:text-destructive"
                disabled={!m.id}
                onSelect={() => onDelete(m)}
              >
                <Trash2 className="h-3.5 w-3.5 opacity-70" />
                {t("message.deleteLocalAction")}
              </ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>

          {!mine && (
            <Actions
              onReply={() => onReply(m)}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onPick={(e) => m.id && onReact(m.id, e)}
              disabled={!m.id || readOnly}
            />
          )}
        </div>

        {m.replyTo && (
          <button
            type="button"
            data-testid="message-parent-snippet"
            aria-label={t("messageFavorites.viewOriginal")}
            title={t("messageFavorites.viewOriginal")}
            aria-busy={parentNavigationPending}
            disabled={!onNavigateToParent || parentNavigationPending}
            onClick={() => m.replyTo && onNavigateToParent?.(m.replyTo)}
            className="mt-1 flex min-h-9 max-w-full items-center gap-2 rounded-md bg-muted px-2.5 py-2 text-left text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default motion-reduce:transition-none"
          >
            {parentNavigationPending ? (
              <span role="status">{t("common.loading")}</span>
            ) : parent ? (
              <ReplyPreview message={parent} authorName={parentAuthorName} />
            ) : (
              <span>{t("messageFavorites.viewOriginal")}</span>
            )}
          </button>
        )}

        {/* reaction chips */}
        {reactions.length > 0 && (
          <div
            className={cn("mt-1 flex flex-wrap gap-1", mine && "justify-end")}
          >
            {reactions.map((r) => {
              const me = reactionFromMe(r, selfReactionId);
              return (
                <button
                  key={`${r.target}:${r.emoji}`}
                  type="button"
                  data-testid="reaction-chip"
                  aria-label={t(
                    me ? "message.reactionRemove" : "message.reactionAdd",
                    { emoji: r.emoji },
                  )}
                  onClick={() => m.id && onReact(m.id, r.emoji)}
                  disabled={readOnly}
                  className={cn(
                    "flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    me
                      ? "border-signal bg-signal/15 text-signal"
                      : "border-border bg-muted/50 hover:bg-muted",
                  )}
                >
                  <span>{r.emoji}</span>
                  <span className="font-mono text-[11px] tabular-nums">
                    {r.who.length}
                  </span>
                </button>
              );
            })}
          </div>
        )}

        <DeliveryFooter
          message={m}
          isChannel={isChannel}
          isPersonal={isPersonal}
          showTime={showTime}
        />

        {m.failed && (
          <span
            className={cn(
              "mt-0.5 flex items-center gap-1.5 px-1 text-[11px] text-destructive",
              mine && "flex-row-reverse",
            )}
          >
            {!(m.file && m.metadataPending) && (
              <span>{t(`message.fail.${m.failReason ?? "unknown"}`)}</span>
            )}
            <button
              type="button"
              onClick={() => m.clientId && onRetry(m.clientId)}
              disabled={!m.clientId || readOnly}
              aria-label={t("message.retry")}
              className="inline-flex items-center gap-0.5 rounded px-1 py-0.5 font-medium text-destructive underline-offset-2 hover:bg-destructive/10 hover:underline disabled:opacity-40"
            >
              <RotateCw className="h-2.5 w-2.5" />
              {t("message.retry")}
            </button>
          </span>
        )}
      </div>
    </motion.div>
  );
}

function Actions({
  onReply,
  onPick,
  pickerOpen,
  setPickerOpen,
  disabled,
}: {
  onReply: () => void;
  onPick: (emoji: string) => void;
  pickerOpen: boolean;
  setPickerOpen: (v: boolean) => void;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  const [focused, setFocused] = useState(false);
  return (
    <div
      onFocusCapture={() => setFocused(true)}
      onBlurCapture={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
          setFocused(false);
        }
      }}
      style={focused ? { opacity: 1 } : undefined}
      className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 [@media(hover:none)]:opacity-100"
    >
      <button
        onClick={onReply}
        disabled={disabled}
        data-testid="message-reply"
        title={t("message.reply")}
        aria-label={t("message.reply")}
        className="flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-30 [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
      >
        <CornerUpLeft className="h-3.5 w-3.5" />
      </button>
      <Popover open={pickerOpen} onOpenChange={setPickerOpen}>
        <PopoverTrigger asChild>
          <button
            disabled={disabled}
            data-testid="message-react"
            title={t("message.react")}
            aria-label={t("message.react")}
            className="flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-30 [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
          >
            <SmilePlus className="h-3.5 w-3.5" />
          </button>
        </PopoverTrigger>
        <PopoverContent className="w-auto" data-testid="reaction-picker">
          <div className="flex gap-1">
            {EMOJIS.map((e) => (
              <button
                key={e}
                data-testid={`reaction-option-${e}`}
                onClick={() => {
                  onPick(e);
                  setPickerOpen(false);
                }}
                className="rounded-md p-1 text-lg hover:bg-accent"
              >
                {e}
              </button>
            ))}
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}
