import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import {
  Copy,
  CornerUpLeft,
  Download,
  RotateCw,
  SmilePlus,
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
import { errorMessage } from "@/lib/error";
import { formatTime, humanSize, shortId } from "@/lib/format";
import { fadeSlideUp } from "@/lib/motion";
import { EMOJIS, mentionsName, renderWithMentions } from "@/lib/mentions";
import type { ReactionInfo } from "@/lib/types";
import { useChat, type ChatMessage } from "@/store/chat";
import { MediaPreview } from "./MediaPreview";
import { fileGlyph } from "./mediaFile";

/** A file/media message body: inline media for images/small videos, else a file card with
 * a Save action. Reuses the shared MediaPreview (lazy bytes + revoked blob URLs). */
function FileBubble({
  file,
  mine,
}: {
  file: NonNullable<ChatMessage["file"]>;
  mine: boolean;
}) {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  // Inline preview ONLY for files sent via the media button (sender intent), not by file
  // extension — so a .mov sent as an attachment renders as a file card, not an inline player.
  const media = file.media;

  const saveAs = async () => {
    try {
      const dest = await save({ defaultPath: file.name });
      if (typeof dest === "string") await chat.saveFile(file.fileConv, dest);
    } catch (e) {
      setError(t("files.couldntSave", { error: errorMessage(e) }));
    }
  };

  return (
    <div className="min-w-[12rem] max-w-xs">
      {media && (
        <MediaPreview
          fileConv={file.fileConv}
          name={file.name}
          size={file.size}
          mime={file.mime}
        />
      )}
      <div className="flex items-center gap-2">
        {fileGlyph(file.name)}
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm">{file.name}</div>
          <div className="font-mono text-[10px] opacity-70">
            {humanSize(file.size)}
          </div>
        </div>
        <button
          type="button"
          onClick={() => void saveAs()}
          title={t("common.save")}
          aria-label={t("common.save")}
          className={cn(
            "rounded-md p-1 transition-colors",
            mine
              ? "text-primary-foreground/80 hover:bg-primary-foreground/15"
              : "text-muted-foreground hover:bg-accent hover:text-foreground",
          )}
        >
          <Download className="h-4 w-4" />
        </button>
      </div>
    </div>
  );
}

export function MessageBubble({
  m,
  parent,
  showAuthor,
  isChannel,
  authorAvatarId,
  authorName,
  fresh,
  reactions,
  selfReactionId,
  myName,
  onReply,
  onReact,
  onRetry,
}: {
  m: ChatMessage;
  parent: ChatMessage | null;
  showAuthor: boolean;
  /** Channel conversations show per-message author avatar + name; DMs don't. */
  isChannel: boolean;
  /** The author's ACCOUNT id (resolved from the roster) for avatar lookup; defaults to
   *  the device id (m.who) when unknown. Only used for the channel author glyph. */
  authorAvatarId?: string;
  /** The author's display NAME (resolved from the roster/members); falls back to the raw
   *  user-id when unknown. Only used for the channel author label. */
  authorName?: string;
  /** True only for a genuinely newly-arriving/sent message → plays an entrance. */
  fresh?: boolean;
  reactions: ReactionInfo[];
  // The id that represents "me" in a reaction's `who`: the ACCOUNT id for account
  // conversations (who is account-keyed there), the device user-id for channels.
  selfReactionId: string;
  myName: string;
  onReply: (m: ChatMessage) => void;
  onReact: (target: string, emoji: string) => void;
  onRetry: (clientId: string) => void;
}) {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const mine = m.fromMe;
  const isFile = !!m.file;
  const [pickerOpen, setPickerOpen] = useState(false);
  // Copy the message text to the clipboard. File messages have no text → no-op.
  const copyText = async () => {
    if (!m.text) return;
    try {
      await navigator.clipboard.writeText(m.text);
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  const mentioned = !mine && mentionsName(m.text, myName);
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
  const body = isFile ? t("files.sentFile", { name: m.file!.name }) : m.text;
  const ariaLabel = `${speaker}: ${body} · ${formatTime(m.wallClock)}${
    status ? ` · ${status}` : ""
  }`;

  return (
    <motion.div
      role="article"
      data-testid="message-bubble"
      aria-label={ariaLabel}
      initial={fresh ? "hidden" : false}
      animate="visible"
      variants={fadeSlideUp}
      className={cn(
        "group flex gap-2.5 px-4 py-0.5",
        mine && "flex-row-reverse",
      )}
    >
      {/* Avatar gutter only in channels; DMs align flush with no empty gutter. */}
      {isChannel && (
        <div className="w-8 shrink-0">
          {showAuthor && !mine && (
            // Seed by the author's ACCOUNT id (resolved from the roster) so a propagated/
            // custom avatar resolves — avatars are account-keyed, m.who is a device id.
            // Falls back to m.who when the account isn't known.
            <IdentityGlyph
              seed={authorAvatarId ?? m.who}
              size={32}
              title={authorLabel}
            />
          )}
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
        {showAuthor && !mine && (
          <span
            className={cn(
              "mb-1 px-1 text-xs font-medium text-muted-foreground",
              isIdName ? "font-mono" : "font-display tracking-tight",
            )}
          >
            {authorLabel}
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
              disabled={!m.id}
            />
          )}

          <ContextMenu>
            <ContextMenuTrigger asChild>
              <div
                // Marks this region so the prod global contextmenu-disable lets our
                // custom menu open here (see main.tsx).
                data-context-menu=""
                className={cn(
                  "min-w-0 rounded-2xl px-3.5 py-2 text-sm transition-shadow",
                  mine
                    ? "rounded-br-md bg-bubble-own text-primary-foreground shadow-sm"
                    : "rounded-bl-md border border-border bg-muted text-foreground shadow-elevation",
                  mentioned && "ring-2 ring-mention/70",
                  m.pending && "opacity-60",
                )}
              >
                {parent && (
                  <div
                    data-testid="message-parent-snippet"
                    className={cn(
                      "mb-1 flex items-center gap-1 rounded-md border-l-2 px-2 py-1 text-xs",
                      mine
                        ? "border-primary-foreground/40 bg-primary-foreground/10"
                        : "border-signal/50 bg-muted/50",
                    )}
                  >
                    <CornerUpLeft className="h-3 w-3 shrink-0 opacity-70" />
                    <span className="truncate opacity-80">
                      {parent.text || t("composer.message")}
                    </span>
                  </div>
                )}
                {isFile ? (
                  <FileBubble file={m.file!} mine={mine} />
                ) : (
                  <span className="cursor-text select-text whitespace-pre-wrap [overflow-wrap:anywhere]">
                    {renderWithMentions(m.text)}
                  </span>
                )}
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent data-testid="message-context-menu">
              <ContextMenuItem
                data-testid="msg-copy"
                disabled={!m.text}
                onSelect={() => void copyText()}
              >
                <Copy className="h-3.5 w-3.5 opacity-70" />
                {t("common.copy")}
              </ContextMenuItem>
              <ContextMenuItem
                data-testid="msg-reply"
                disabled={!m.id}
                onSelect={() => onReply(m)}
              >
                <CornerUpLeft className="h-3.5 w-3.5 opacity-70" />
                {t("message.reply")}
              </ContextMenuItem>
              <ContextMenuItem
                data-testid="msg-react"
                disabled={!m.id}
                // Defer so the menu finishes closing before the picker popover opens
                // (avoids a focus/layer fight between the two radix overlays).
                onSelect={() => setTimeout(() => setPickerOpen(true), 0)}
              >
                <SmilePlus className="h-3.5 w-3.5 opacity-70" />
                {t("message.react")}
              </ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>

          {!mine && (
            <Actions
              onReply={() => onReply(m)}
              pickerOpen={pickerOpen}
              setPickerOpen={setPickerOpen}
              onPick={(e) => m.id && onReact(m.id, e)}
              disabled={!m.id}
            />
          )}
        </div>

        {/* reaction chips */}
        {reactions.length > 0 && (
          <div
            className={cn("mt-1 flex flex-wrap gap-1", mine && "justify-end")}
          >
            {reactions.map((r) => {
              const me = r.who.includes(selfReactionId);
              return (
                <button
                  key={`${r.target}:${r.emoji}`}
                  type="button"
                  data-testid="reaction-chip"
                  aria-label={`${me ? "Remove your" : "Add"} ${r.emoji} reaction`}
                  onClick={() => m.id && onReact(m.id, r.emoji)}
                  className={cn(
                    "flex items-center gap-1 rounded-full border px-1.5 py-0.5 text-xs transition-colors",
                    me
                      ? "border-signal bg-signal/15 text-signal"
                      : "border-border bg-muted/50 hover:bg-muted",
                  )}
                >
                  <span>{r.emoji}</span>
                  <span className="font-mono text-[10px]">{r.who.length}</span>
                </button>
              );
            })}
          </div>
        )}

        <span className="mt-0.5 px-1 font-mono text-[10px] text-muted-foreground">
          {formatTime(m.wallClock)}
          {m.pending && ` · ${t("message.sending")}`}
        </span>

        {m.failed && (
          <span
            className={cn(
              "mt-0.5 flex items-center gap-1.5 px-1 text-[10px] text-destructive",
              mine && "flex-row-reverse",
            )}
          >
            <span>{t(`message.fail.${m.failReason ?? "unknown"}`)}</span>
            <button
              type="button"
              onClick={() => m.clientId && onRetry(m.clientId)}
              disabled={!m.clientId}
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
  return (
    <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
      <button
        onClick={onReply}
        disabled={disabled}
        data-testid="message-reply"
        title={t("message.reply")}
        aria-label={t("message.reply")}
        className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
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
            className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-30"
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
