import { useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import {
  SendHorizontal,
  X,
  CornerUpLeft,
  Paperclip,
  FolderUp,
  Smile,
  Sticker as StickerIcon,
  Camera,
  Image as ImageIcon,
  Plus,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ease, useMotionOK } from "@/lib/motion";
import { STICKERS, installedStickers } from "@/lib/stickerPacks";
import { usePacks } from "@/store/packs";
import { PackManager } from "@/components/PackManager";
import type { ChatMessage } from "@/store/chat";
import { clipboardFiles } from "./clipboardFiles";

// A small curated palette — enough for everyday chat without pulling in a heavy emoji library.
const EMOJIS = [
  "😀",
  "😂",
  "🙂",
  "😉",
  "😍",
  "😎",
  "🤔",
  "😅",
  "😭",
  "😡",
  "👍",
  "👎",
  "🙏",
  "👏",
  "🙌",
  "💪",
  "👀",
  "👋",
  "🤝",
  "🤙",
  "🔥",
  "🎉",
  "✨",
  "⭐",
  "❤️",
  "💯",
  "✅",
  "❌",
  "🚀",
  "💡",
  "☕",
  "🍻",
  "🥳",
  "🤯",
  "🙈",
  "😴",
  "🎯",
  "📎",
  "💀",
  "🤗",
];

type PanelMotion = { pointer: boolean; reduced: boolean };
const panelVariants = {
  hidden: ({ pointer, reduced }: PanelMotion) => ({
    opacity: pointer ? 0 : 1,
    transform: reduced
      ? "none"
      : pointer
        ? "translateY(4px) scale(1)"
        : "translateY(0px) scale(1)",
    transition: { duration: pointer ? (reduced ? 0.08 : 0.1) : 0, ease },
  }),
  visible: ({ pointer, reduced }: PanelMotion) => ({
    opacity: 1,
    transform: reduced ? "none" : "translateY(0px) scale(1)",
    transition: { duration: pointer ? (reduced ? 0.08 : 0.14) : 0, ease },
  }),
};

function useReducedMotion() {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(media.matches);
    update();
    media.addEventListener?.("change", update);
    return () => media.removeEventListener?.("change", update);
  }, []);
  return reduced;
}

function StickerThumb({
  src,
  alt,
  fallback,
}: {
  src: string;
  alt: string;
  fallback: string;
}) {
  const reducedMotion = useReducedMotion();
  const [thumb, setThumb] = useState<string | null>(null);

  useEffect(() => {
    if (!reducedMotion) return;
    let active = true;
    setThumb(null);

    void (async () => {
      try {
        const blob = await fetch(src).then((res) => res.blob());
        const bitmap = await createImageBitmap(blob);
        const canvas = document.createElement("canvas");
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        const ctx = canvas.getContext("2d");
        if (!ctx) {
          bitmap.close();
          return;
        }
        ctx.drawImage(bitmap, 0, 0);
        bitmap.close();
        if (active) setThumb(canvas.toDataURL("image/webp"));
      } catch {
        if (active) setThumb(null);
      }
    })();

    return () => {
      active = false;
    };
  }, [reducedMotion, src]);

  return reducedMotion ? (
    thumb ? (
      <img
        src={thumb}
        alt={alt}
        loading="eager"
        decoding="async"
        width={48}
        height={48}
        className="h-12 w-12"
        draggable={false}
      />
    ) : (
      <div
        className="flex h-12 w-12 items-center justify-center text-2xl leading-none"
        aria-hidden="true"
      >
        {fallback}
      </div>
    )
  ) : (
    <img
      src={src}
      alt={alt}
      loading="lazy"
      decoding="async"
      width={48}
      height={48}
      className="h-12 w-12"
      draggable={false}
    />
  );
}

export function Composer({
  onSend,
  initialDraft = "",
  onDraftChange,
  onAttach,
  onAttachDirectory,
  onPasteFiles,
  onScreenshot,
  screenshotAvailable = true,
  placeholder,
  replyTo,
  onCancelReply,
  mentionNames,
  myName,
  prefill,
  onSendSticker,
  onImage,
}: {
  onSend: (text: string) => boolean;
  initialDraft?: string;
  onDraftChange?: (text: string) => void;
  onAttach?: () => void;
  onAttachDirectory?: () => void;
  /** Send copied files after the user presses Enter or Send. */
  onPasteFiles?: (files: File[]) => Promise<void>;
  /** Capture a screenshot and send it (hideWindow = hide the app first). */
  onScreenshot?: (hideWindow: boolean) => void;
  screenshotAvailable?: boolean;
  placeholder: string;
  replyTo?: ChatMessage | null;
  onCancelReply?: () => void;
  mentionNames: string[];
  /** The current user's display name — excluded from mention suggestions (can't @ yourself). */
  myName?: string;
  /** Drop text into the composer from outside (WeChat-style "re-edit" of a recalled
   * message). `n` is a bump counter so the same text can be re-applied. */
  prefill?: { text: string; n: number } | null;
  /** Send a bundled sticker (by id, with its emoji fallback) as its own message. */
  onSendSticker?: (stickerId: string, fallback: string) => void;
  /** Pick + send an image/video via the NATIVE file dialog (reliable in the webview;
   * a JS `<input type=file>` is flaky in WKWebView). Paste/screenshot still use bytes. */
  onImage?: () => void;
}) {
  const { t } = useTranslation();
  const installedPacks = usePacks((s) => s.packs);
  const stickers = useMemo(
    () => [...STICKERS, ...installedStickers(installedPacks)],
    [installedPacks],
  );
  const stickerLibraries = installedPacks.filter(
    (pack) => pack.kind === "sticker",
  );
  const motionOK = useMotionOK();
  const [text, setText] = useState(initialDraft);
  const [sendRejected, setSendRejected] = useState(false);
  const [pendingFiles, setPendingFiles] = useState<File[]>([]);
  const [sendingFiles, setSendingFiles] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const [expressionTab, setExpressionTab] = useState<
    "emoji" | "stickers" | null
  >(null);
  const [stickerPack, setStickerPack] = useState("noto");
  const activeStickerPack = stickerLibraries.some(
    (pack) => pack.id === stickerPack,
  )
    ? stickerPack
    : "noto";
  const [showTools, setShowTools] = useState(false);
  const [panelPointerMotion, setPanelPointerMotion] = useState(false);
  const panelMotion = { pointer: panelPointerMotion, reduced: !motionOK };

  // Apply an external prefill (re-edit): replace the draft and focus, caret at the end.
  // Keyed on the bump counter so re-editing the same text twice still re-applies.
  useEffect(() => {
    if (!prefill) return;
    setText(prefill.text);
    onDraftChange?.(prefill.text);
    queueMicrotask(() => {
      const el = ref.current;
      if (el) {
        el.focus();
        el.setSelectionRange(prefill.text.length, prefill.text.length);
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefill?.n]);
  const [mentionQuery, setMentionQuery] = useState<string | null>(null);
  const [showShot, setShowShot] = useState(false);

  // Dismiss the expression / screenshot panels on an outside click or Escape — they're
  // plain popovers, so without this they'd only close by clicking their button again. Panels
  // AND their toggle buttons carry `data-composer-popover`, so a click on a toggle is ignored
  // here and left to the button's own handler (no close-then-reopen fight).
  useEffect(() => {
    if (!expressionTab && !showShot) return;
    const close = () => {
      setExpressionTab(null);
      setShowShot(false);
    };
    const onPointerDown = (e: PointerEvent) => {
      if (!(e.target as Element | null)?.closest("[data-composer-popover]")) {
        setPanelPointerMotion(true);
        close();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const focusedPanel = (document.activeElement as Element | null)?.closest(
        "#composer-expression-panel, #composer-screenshot-panel",
      );
      const returnTo = focusedPanel
        ? document.querySelector<HTMLButtonElement>(
            showShot
              ? '[data-testid="composer-screenshot"]'
              : '[data-testid="composer-emoji"]',
          )
        : null;
      setPanelPointerMotion(false);
      close();
      if (returnTo) queueMicrotask(() => returnTo.focus());
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [expressionTab, showShot]);

  const insertEmoji = (emoji: string) => {
    const el = ref.current;
    const caret = el?.selectionStart ?? text.length;
    const next = text.slice(0, caret) + emoji + text.slice(caret);
    setText(next);
    onDraftChange?.(next);
    setPanelPointerMotion(false);
    setExpressionTab(null);
    setShowShot(false);
    queueMicrotask(() => {
      el?.focus();
      const pos = caret + emoji.length;
      el?.setSelectionRange(pos, pos);
    });
  };

  const suggestions = useMemo(() => {
    if (mentionQuery === null) return [];
    const q = mentionQuery.toLowerCase();
    return mentionNames
      .filter((n) => n && n.toLowerCase().startsWith(q) && n !== myName)
      .slice(0, 6);
  }, [mentionQuery, mentionNames, myName]);

  const detectMention = (value: string, caret: number) => {
    const before = value.slice(0, caret);
    const m = before.match(/@([\p{L}\p{N}_-]*)$/u);
    setMentionQuery(m ? m[1] : null);
  };

  const applyMention = (name: string) => {
    const el = ref.current;
    if (!el) return;
    const caret = el.selectionStart ?? text.length;
    const before = text
      .slice(0, caret)
      .replace(/@([\p{L}\p{N}_-]*)$/u, `@${name} `);
    const next = before + text.slice(caret);
    setText(next);
    onDraftChange?.(next);
    setMentionQuery(null);
    queueMicrotask(() => el.focus());
  };

  const send = () => {
    if (pendingFiles.length) {
      if (sendingFiles || !onPasteFiles) return;
      setSendingFiles(true);
      void onPasteFiles(pendingFiles)
        .then(
          () => setPendingFiles([]),
          () => setSendRejected(true),
        )
        .finally(() => setSendingFiles(false));
      return;
    }
    const t = text.trim();
    if (!t) return;
    if (!onSend(t)) {
      setSendRejected(true);
      return;
    }
    setSendRejected(false);
    setText("");
    onDraftChange?.("");
    setMentionQuery(null);
    if (ref.current) ref.current.style.height = "auto";
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Enter confirms an IME candidate in many writing systems. Let the input method
    // finish composition before interpreting Enter as a send shortcut.
    if (e.nativeEvent.isComposing) return;
    if (suggestions.length > 0 && e.key === "Enter") {
      e.preventDefault();
      applyMention(suggestions[0]);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  // Stage copied files (including screenshots) for the normal Enter/Send shortcut.
  // Desktop clipboards often include the filename as text too; consume that only when
  // actual file bytes are present. Ordinary text paste still uses the browser default.
  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!onPasteFiles) return;
    const files = clipboardFiles(e.clipboardData);
    if (!files.length) return;
    e.preventDefault();
    setPendingFiles((current) => [...current, ...files]);
    setSendRejected(false);
  };

  const onInput = (e: React.FormEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    setSendRejected(false);
    setText(el.value);
    onDraftChange?.(el.value);
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    detectMention(el.value, el.selectionStart ?? el.value.length);
  };

  return (
    <div
      data-testid="composer"
      className="composer-dock border-t bg-[hsl(var(--composer-surface))] px-4 pb-3 pt-3"
    >
      {replyTo && (
        <div
          data-testid="composer-reply-banner"
          className="mx-auto mb-2 flex max-w-[820px] items-center gap-2 rounded-md bg-muted px-3 py-1.5 text-[13px]"
        >
          <CornerUpLeft className="h-3.5 w-3.5 shrink-0 text-signal" />
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            {t("composer.replyingTo")}{" "}
            <span className="text-foreground">
              {replyTo.text || t("composer.message")}
            </span>
          </span>
          <button
            onClick={onCancelReply}
            type="button"
            aria-label={t("composer.cancelReply")}
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      {pendingFiles.length > 0 && (
        <div
          data-testid="composer-pending-files"
          className="mx-auto mb-2 flex max-w-[820px] flex-wrap gap-2"
        >
          {pendingFiles.map((file, index) => (
            <span
              key={`${file.name}-${index}`}
              className="flex max-w-full items-center gap-1 rounded-md bg-muted px-2 py-1 text-xs"
            >
              <Paperclip className="h-3.5 w-3.5 shrink-0" />
              <span className="truncate" title={file.name}>
                {file.name}
              </span>
              <button
                type="button"
                disabled={sendingFiles}
                aria-label={`${t("common.dismiss")} ${file.name}`}
                onClick={() =>
                  setPendingFiles((files) =>
                    files.filter((_, i) => i !== index),
                  )
                }
                className="rounded p-1 hover:bg-accent"
              >
                <X className="h-3 w-3" />
              </button>
            </span>
          ))}
        </div>
      )}

      <div className="group relative mx-auto max-w-[820px]">
        {suggestions.length > 0 && (
          <div
            data-testid="mention-popover"
            className="absolute bottom-full left-0 mb-2 w-56 overflow-hidden rounded-lg border bg-popover shadow-elevation"
          >
            {suggestions.map((n) => (
              <button
                key={n}
                data-testid={`mention-option-${n}`}
                onClick={() => applyMention(n)}
                className="block w-full px-3 py-2 text-left text-sm hover:bg-accent"
              >
                <span className="font-medium text-mention">@</span>
                {n}
              </button>
            ))}
          </div>
        )}

        <AnimatePresence custom={panelMotion}>
          {expressionTab && (
            <motion.div
              id="composer-expression-panel"
              data-testid="expression-picker"
              data-composer-popover=""
              custom={panelMotion}
              variants={panelVariants}
              initial="hidden"
              animate="visible"
              exit="hidden"
              style={{ transformOrigin: "left bottom" }}
              className="absolute bottom-full left-0 mb-2 flex max-h-80 w-80 max-w-full flex-col overflow-hidden rounded-lg border bg-popover shadow-elevation"
            >
              <div
                className="flex gap-1 border-b px-2 pt-2"
                role="tablist"
                aria-label={`${t("composer.emoji")} / ${t("composer.stickers")}`}
              >
                <button
                  type="button"
                  role="tab"
                  id="composer-emoji-tab"
                  data-testid="composer-emoji-tab"
                  aria-selected={expressionTab === "emoji"}
                  tabIndex={expressionTab === "emoji" ? 0 : -1}
                  aria-controls="composer-emoji-panel"
                  onClick={() => setExpressionTab("emoji")}
                  onKeyDown={(event) => {
                    if (event.key === "ArrowRight" && onSendSticker) {
                      event.preventDefault();
                      setExpressionTab("stickers");
                      document.getElementById("composer-sticker-tab")?.focus();
                    }
                  }}
                  className={cn(
                    "flex min-h-9 items-center gap-2 border-b-2 px-3 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    expressionTab === "emoji"
                      ? "border-signal text-foreground"
                      : "border-transparent text-muted-foreground hover:text-foreground",
                  )}
                >
                  <Smile className="h-4 w-4" />
                  {t("composer.emoji")}
                </button>
                {onSendSticker && (
                  <button
                    type="button"
                    role="tab"
                    id="composer-sticker-tab"
                    data-testid="composer-stickers"
                    aria-selected={expressionTab === "stickers"}
                    tabIndex={expressionTab === "stickers" ? 0 : -1}
                    aria-controls="composer-sticker-panel"
                    onClick={() => setExpressionTab("stickers")}
                    onKeyDown={(event) => {
                      if (event.key === "ArrowLeft") {
                        event.preventDefault();
                        setExpressionTab("emoji");
                        document.getElementById("composer-emoji-tab")?.focus();
                      }
                    }}
                    className={cn(
                      "flex min-h-9 items-center gap-2 border-b-2 px-3 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      expressionTab === "stickers"
                        ? "border-signal text-foreground"
                        : "border-transparent text-muted-foreground hover:text-foreground",
                    )}
                  >
                    <StickerIcon className="h-4 w-4" />
                    {t("composer.stickers")}
                  </button>
                )}
              </div>
              {expressionTab === "emoji" ? (
                <div
                  id="composer-emoji-panel"
                  role="tabpanel"
                  aria-labelledby="composer-emoji-tab"
                  data-testid="emoji-picker"
                  className="overflow-y-auto p-2"
                >
                  <div className="grid grid-cols-10 gap-0.5">
                    {EMOJIS.map((e) => (
                      <button
                        key={e}
                        type="button"
                        data-testid={`emoji-option-${e}`}
                        onClick={() => insertEmoji(e)}
                        className="rounded-md p-1 text-lg leading-none hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                        aria-label={t("composer.insertEmoji", { emoji: e })}
                      >
                        {e}
                      </button>
                    ))}
                  </div>
                </div>
              ) : onSendSticker ? (
                <div
                  id="composer-sticker-panel"
                  role="tabpanel"
                  aria-labelledby="composer-sticker-tab"
                  data-testid="sticker-panel"
                  className="overflow-y-auto p-2"
                >
                  <div
                    className="mb-2 flex gap-1 overflow-x-auto border-b pb-2"
                    aria-label={t("composer.stickerPacks")}
                  >
                    {[
                      { id: "noto", name: t("composer.stickerPackEmoji") },
                      ...stickerLibraries.map((pack) => ({
                        id: pack.id,
                        name:
                          pack.id === "cats" && pack.name === "Cat Stickers"
                            ? t("composer.stickerPackCats")
                            : pack.name,
                      })),
                    ].map((pack) => (
                      <button
                        key={pack.id}
                        type="button"
                        data-testid={`sticker-pack-${pack.id}`}
                        aria-pressed={activeStickerPack === pack.id}
                        onClick={() => setStickerPack(pack.id)}
                        className={cn(
                          "shrink-0 rounded-md px-2 py-1.5 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                          activeStickerPack === pack.id
                            ? "bg-accent text-foreground"
                            : "text-muted-foreground hover:bg-accent/70 hover:text-foreground",
                        )}
                      >
                        {pack.name}
                      </button>
                    ))}
                  </div>
                  <div className="grid grid-cols-5 gap-1">
                    {stickers
                      .filter((sticker) =>
                        activeStickerPack === "noto"
                          ? !sticker.id.startsWith("pack:")
                          : sticker.id.startsWith(`pack:${activeStickerPack}:`),
                      )
                      .map((s) => (
                        <button
                          key={s.id}
                          type="button"
                          data-testid={`sticker-option-${s.id}`}
                          onClick={() => {
                            onSendSticker(s.id, s.emoji);
                            setPanelPointerMotion(false);
                            setExpressionTab(null);
                          }}
                          className="rounded-lg p-1 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                          title={s.label ?? s.emoji}
                          aria-label={t("composer.sendSticker", {
                            emoji: s.label ?? s.emoji,
                          })}
                        >
                          <StickerThumb
                            src={s.url}
                            alt={s.label ?? s.emoji}
                            fallback={s.emoji}
                          />
                        </button>
                      ))}
                  </div>
                  <details className="mt-2 border-t border-border pt-2 text-xs">
                    <summary className="cursor-pointer text-primary">
                      {t("composer.manageStickers")}
                    </summary>
                    <div className="pt-3">
                      <PackManager kind="sticker" />
                    </div>
                  </details>
                </div>
              ) : null}
            </motion.div>
          )}
        </AnimatePresence>

        <AnimatePresence custom={panelMotion}>
          {showShot && onScreenshot && (
            <motion.div
              id="composer-screenshot-panel"
              data-testid="screenshot-menu"
              data-composer-popover=""
              custom={panelMotion}
              variants={panelVariants}
              initial="hidden"
              animate="visible"
              exit="hidden"
              style={{ transformOrigin: "left bottom" }}
              className="absolute bottom-full left-0 mb-2 w-56 max-w-full overflow-hidden rounded-lg border bg-popover p-1 shadow-elevation"
            >
              <button
                type="button"
                data-testid="screenshot-now"
                onClick={() => {
                  setPanelPointerMotion(false);
                  setShowShot(false);
                  onScreenshot(false);
                }}
                className="block w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {t("screenshot.now")}
              </button>
              <button
                type="button"
                data-testid="screenshot-hidden"
                onClick={() => {
                  setPanelPointerMotion(false);
                  setShowShot(false);
                  onScreenshot(true);
                }}
                className="block w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {t("screenshot.hidden")}
              </button>
            </motion.div>
          )}
        </AnimatePresence>

        {/* Secondary tools open above the input when requested. */}
        <div
          className={cn("mb-2 flex items-center gap-1", !showTools && "hidden")}
          role="toolbar"
          aria-label={t("composer.tools")}
        >
          {onScreenshot && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-screenshot"
              data-composer-popover=""
              disabled={!screenshotAvailable}
              className={cn(
                "h-9 w-9 shrink-0 rounded-md text-muted-foreground",
                showShot && "bg-accent text-foreground",
              )}
              title={t(
                screenshotAvailable
                  ? "screenshot.trigger"
                  : "screenshot.unavailable",
              )}
              aria-label={t("screenshot.trigger")}
              aria-expanded={showShot}
              aria-controls="composer-screenshot-panel"
              onClick={(event) => {
                setPanelPointerMotion(event.detail !== 0);
                setExpressionTab(null);
                setShowShot((v) => !v);
              }}
            >
              <Camera className="h-4 w-4" />
            </Button>
          )}
          {onAttach && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-attach"
              className="h-9 w-9 shrink-0 rounded-md text-muted-foreground"
              title={t("composer.attach")}
              aria-label={t("composer.attach")}
              onClick={onAttach}
            >
              <Paperclip className="h-4 w-4" />
            </Button>
          )}
          {onAttachDirectory && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-attach-directory"
              className="h-9 w-9 shrink-0 rounded-md text-muted-foreground"
              title={t("composer.attachDirectory")}
              aria-label={t("composer.attachDirectory")}
              onClick={onAttachDirectory}
            >
              <FolderUp className="h-4 w-4" />
            </Button>
          )}
          {onImage && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-image"
              className="h-9 w-9 shrink-0 rounded-md text-muted-foreground"
              title={t("composer.image")}
              aria-label={t("composer.image")}
              onClick={onImage}
            >
              <ImageIcon className="h-4 w-4" />
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon"
            data-testid="composer-emoji"
            data-composer-popover=""
            className={cn(
              "ml-1 h-9 w-9 shrink-0 rounded-md border-l text-muted-foreground",
              expressionTab && "bg-accent text-foreground",
            )}
            title={`${t("composer.emoji")} / ${t("composer.stickers")}`}
            aria-label={`${t("composer.emoji")} / ${t("composer.stickers")}`}
            aria-expanded={expressionTab !== null}
            aria-controls="composer-expression-panel"
            onClick={(event) => {
              setPanelPointerMotion(event.detail !== 0);
              setShowShot(false);
              setExpressionTab((tab) => (tab ? null : "emoji"));
            }}
          >
            <Smile className="h-4 w-4" />
          </Button>
        </div>

        <div className="flex items-end gap-2 rounded-lg border border-input bg-background p-1.5 transition-[border-color,box-shadow] duration-150 focus-within:border-signal focus-within:ring-1 focus-within:ring-signal/20">
          <Button
            variant="ghost"
            size="icon"
            type="button"
            data-testid="composer-more-tools"
            aria-label={t("redesign.addTools")}
            aria-expanded={showTools}
            onClick={() => {
              if (showTools) {
                setPanelPointerMotion(false);
                setExpressionTab(null);
                setShowShot(false);
              }
              setShowTools((v) => !v);
            }}
            className="h-9 w-9 shrink-0 rounded-lg text-muted-foreground"
          >
            <Plus className={cn("h-4 w-4", showTools && "rotate-45")} />
          </Button>
          <textarea
            ref={ref}
            rows={1}
            data-testid="composer-input"
            value={text}
            onChange={onInput}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            placeholder={placeholder}
            aria-label={placeholder}
            title={t("redesign.keyboardHint")}
            className="max-h-40 flex-1 resize-none bg-transparent px-1 py-1.5 text-[14px] leading-6 outline-none placeholder:text-muted-foreground"
          />
          <Button
            size="icon"
            data-testid="composer-send"
            aria-label={t("composer.send")}
            className={cn(
              "h-9 w-9 shrink-0 rounded-md bg-signal text-primary-foreground",
            )}
            disabled={
              sendingFiles || (!text.trim() && pendingFiles.length === 0)
            }
            onClick={() => {
              send();
              ref.current?.focus();
            }}
          >
            <SendHorizontal className="h-4 w-4" />
          </Button>
        </div>
        {sendRejected ? (
          <p
            role="alert"
            className="mt-1.5 min-h-4 text-right text-[11px] text-destructive"
          >
            {t("composer.notQueued")}
          </p>
        ) : (
          <p className="mt-1.5 h-4 text-right text-[11px] text-muted-foreground opacity-0 transition-opacity group-focus-within:opacity-100">
            {t("redesign.keyboardHint")}
          </p>
        )}
      </div>
    </div>
  );
}
