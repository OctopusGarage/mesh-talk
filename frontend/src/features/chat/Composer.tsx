import { useEffect, useMemo, useRef, useState } from "react";
import {
  SendHorizontal,
  X,
  CornerUpLeft,
  Paperclip,
  Smile,
  Camera,
  Image as ImageIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { pickImageFile } from "@/lib/avatarImage";
import type { ChatMessage } from "@/store/chat";

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

export function Composer({
  onSend,
  onAttach,
  onPasteImage,
  onScreenshot,
  placeholder,
  replyTo,
  onCancelReply,
  mentionNames,
  prefill,
}: {
  onSend: (text: string) => void;
  onAttach?: () => void;
  /** Send an image pasted from the clipboard (bytes + file extension). */
  onPasteImage?: (bytes: Uint8Array, ext: string, name?: string) => void;
  /** Capture a screenshot and send it (hideWindow = hide the app first). */
  onScreenshot?: (hideWindow: boolean) => void;
  placeholder: string;
  replyTo?: ChatMessage | null;
  onCancelReply?: () => void;
  mentionNames: string[];
  /** Drop text into the composer from outside (WeChat-style "re-edit" of a recalled
   * message). `n` is a bump counter so the same text can be re-applied. */
  prefill?: { text: string; n: number } | null;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const ref = useRef<HTMLTextAreaElement>(null);

  // Apply an external prefill (re-edit): replace the draft and focus, caret at the end.
  // Keyed on the bump counter so re-editing the same text twice still re-applies.
  useEffect(() => {
    if (!prefill) return;
    setText(prefill.text);
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
  const [showEmoji, setShowEmoji] = useState(false);
  const [showShot, setShowShot] = useState(false);

  const insertEmoji = (emoji: string) => {
    const el = ref.current;
    const caret = el?.selectionStart ?? text.length;
    setText(text.slice(0, caret) + emoji + text.slice(caret));
    setShowEmoji(false);
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
      .filter((n) => n && n.toLowerCase().startsWith(q))
      .slice(0, 6);
  }, [mentionQuery, mentionNames]);

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
    setMentionQuery(null);
    queueMicrotask(() => el.focus());
  };

  const send = () => {
    const t = text.trim();
    if (!t) return;
    onSend(t);
    setText("");
    setMentionQuery(null);
    if (ref.current) ref.current.style.height = "auto";
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
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

  // Paste-to-attach: if the clipboard carries an image (e.g. a screenshot), send it as a
  // file instead of pasting nothing. Text paste falls through to the default behavior.
  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!onPasteImage) return;
    const item = Array.from(e.clipboardData.items).find((i) =>
      i.type.startsWith("image/"),
    );
    if (!item) return;
    const file = item.getAsFile();
    if (!file) return;
    e.preventDefault();
    const ext = (file.type.split("/")[1] || "png").toLowerCase();
    void file
      .arrayBuffer()
      .then((buf) => onPasteImage(new Uint8Array(buf), ext))
      .catch(() => {});
  };

  // Dedicated "send image/video" button: pick a media file and send it through the same path
  // as a pasted/screenshot image (renders inline). Separate from the generic attach (any file).
  // Accepts video too (a .mov etc. previews inline); the REAL filename is passed through, so
  // the chat shows "clip.mov" — not a MIME-derived "pasted-….quicktim" — and it previews.
  const pickImage = async () => {
    if (!onPasteImage) return;
    const file = await pickImageFile("image/*,video/*");
    if (!file) return;
    // Prefer the file's own extension (a real .mov stays .mov); fall back to the MIME subtype.
    const ext = (
      file.name.split(".").pop() ||
      file.type.split("/")[1] ||
      "png"
    ).toLowerCase();
    const buf = await file.arrayBuffer();
    onPasteImage(new Uint8Array(buf), ext, file.name);
  };

  const onInput = (e: React.FormEvent<HTMLTextAreaElement>) => {
    const el = e.currentTarget;
    setText(el.value);
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    detectMention(el.value, el.selectionStart ?? el.value.length);
  };

  return (
    <div data-testid="composer" className="border-t bg-card/40 p-3">
      {replyTo && (
        <div
          data-testid="composer-reply-banner"
          className="mb-2 flex items-center gap-2 rounded-lg border-l-2 border-signal bg-muted/50 px-3 py-1.5 text-sm"
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
            className="rounded p-0.5 text-muted-foreground hover:bg-accent"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      )}

      <div className="relative">
        {suggestions.length > 0 && (
          <div
            data-testid="mention-popover"
            className="absolute bottom-full left-0 mb-2 w-56 overflow-hidden rounded-xl border bg-popover shadow-elevation"
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

        {showEmoji && (
          <div
            data-testid="emoji-picker"
            className="absolute bottom-full left-0 mb-2 w-72 rounded-xl border bg-popover p-2 shadow-elevation"
          >
            <div className="grid grid-cols-10 gap-0.5">
              {EMOJIS.map((e) => (
                <button
                  key={e}
                  type="button"
                  data-testid={`emoji-option-${e}`}
                  onClick={() => insertEmoji(e)}
                  className="rounded-md p-1 text-lg leading-none hover:bg-accent"
                  aria-label={t("composer.insertEmoji", { emoji: e })}
                >
                  {e}
                </button>
              ))}
            </div>
          </div>
        )}

        {showShot && onScreenshot && (
          <div
            data-testid="screenshot-menu"
            className="absolute bottom-full left-0 mb-2 w-56 overflow-hidden rounded-xl border bg-popover p-1 shadow-elevation"
          >
            <button
              type="button"
              data-testid="screenshot-now"
              onClick={() => {
                setShowShot(false);
                onScreenshot(false);
              }}
              className="block w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-accent"
            >
              {t("screenshot.now")}
            </button>
            <button
              type="button"
              data-testid="screenshot-hidden"
              onClick={() => {
                setShowShot(false);
                onScreenshot(true);
              }}
              className="block w-full rounded-lg px-3 py-2 text-left text-sm hover:bg-accent"
            >
              {t("screenshot.hidden")}
            </button>
          </div>
        )}

        {/* Action toolbar — kept ABOVE the input so the buttons aren't crammed onto the
            typing line (screenshot · attach · image · emoji). */}
        <div className="mb-1.5 flex items-center gap-1">
          {onScreenshot && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-screenshot"
              className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
              title={t("screenshot.trigger")}
              aria-label={t("screenshot.trigger")}
              onClick={() => {
                setShowEmoji(false);
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
              className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
              title={t("composer.attach")}
              aria-label={t("composer.attach")}
              onClick={onAttach}
            >
              <Paperclip className="h-4 w-4" />
            </Button>
          )}
          {onPasteImage && (
            <Button
              variant="ghost"
              size="icon"
              data-testid="composer-image"
              className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
              title={t("composer.image")}
              aria-label={t("composer.image")}
              onClick={() => void pickImage()}
            >
              <ImageIcon className="h-4 w-4" />
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon"
            data-testid="composer-emoji"
            className="h-9 w-9 shrink-0 rounded-xl text-muted-foreground"
            title={t("composer.emoji")}
            aria-label={t("composer.emoji")}
            onClick={() => {
              setShowShot(false);
              setShowEmoji((v) => !v);
            }}
          >
            <Smile className="h-4 w-4" />
          </Button>
        </div>

        <div className="flex items-end gap-2 rounded-2xl border bg-background p-2 transition-colors focus-within:border-signal focus-within:ring-2 focus-within:ring-signal/30">
          <textarea
            ref={ref}
            rows={1}
            data-testid="composer-input"
            value={text}
            onChange={onInput}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            placeholder={placeholder}
            className="max-h-40 flex-1 resize-none bg-transparent px-1 py-1.5 text-sm outline-none placeholder:text-muted-foreground"
          />
          <Button
            size="icon"
            data-testid="composer-send"
            aria-label={t("composer.send")}
            className={cn(
              "h-9 w-9 shrink-0 rounded-xl bg-signal text-primary-foreground transition-opacity",
              !text.trim() && "opacity-40",
            )}
            disabled={!text.trim()}
            onClick={send}
          >
            <SendHorizontal className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  );
}
