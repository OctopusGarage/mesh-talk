import { useEffect, useMemo, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  FileX,
  History,
  Image as ImageIcon,
  Link as LinkIcon,
  Search,
  SearchX,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { IdentityGlyph } from "@/components/identity";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { formatDay, formatTime, humanSize } from "@/lib/format";
import { renderWithMentions } from "@/lib/mentions";
import { categorize, matchesQuery } from "@/lib/conversationHistory";
import {
  convKey,
  fromHistoryItem,
  useChat,
  type ChatMessage,
  type Conversation,
} from "@/store/chat";
import { MediaPreview } from "./MediaPreview";
import { fileGlyph } from "./mediaFile";
import { ClearHistoryButton } from "./ClearHistoryButton";

// Per-conversation history pulls the full backlog (the conversation stream itself caps at
// 200 for live scrolling). The backend history commands clamp to 500, which is the most a
// single page can return — so this view is complete up to the durable-store window.
const FULL_HISTORY_LIMIT = 500;

/** Highlight every case-insensitive occurrence of `term` in `text` (mirrors SearchDialog). */
function Highlighted({ text, term }: { text: string; term: string }) {
  const parts = useMemo(() => {
    const q = term.trim();
    if (!q) return [text];
    const re = new RegExp(
      `(${q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`,
      "gi",
    );
    return text.split(re);
  }, [text, term]);
  return (
    <>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark
            key={i}
            className="rounded-sm bg-signal/20 px-0.5 font-medium text-signal"
          >
            {p}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </>
  );
}

/** Author + timestamp line shared by the text + link rows. */
function RowMeta({ m }: { m: ChatMessage }) {
  const { t } = useTranslation();
  return (
    <div className="mb-0.5 flex items-center gap-2 text-xs text-muted-foreground">
      <IdentityGlyph
        seed={m.fromMe ? "me" : m.who}
        size={16}
        className="shrink-0"
      />
      <span className="truncate font-medium text-foreground/70">
        {m.fromMe ? t("common.you") : m.who || t("common.unnamed")}
      </span>
      <span aria-hidden>·</span>
      <span className="shrink-0 font-mono">
        {formatDay(m.wallClock)} {formatTime(m.wallClock)}
      </span>
    </div>
  );
}

function EmptyTab({ icon, label }: { icon: React.ReactNode; label: string }) {
  return (
    <div className="flex flex-col items-center gap-2 py-10 text-center text-muted-foreground/70">
      {icon}
      <p className="text-sm">{label}</p>
    </div>
  );
}

export function ConversationHistoryDialog({
  conversation,
}: {
  conversation: Conversation;
}) {
  const { t } = useTranslation();
  const ready = useChat((s) => s.ready);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<ChatMessage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const key = convKey(conversation);

  // Load the full conversation history when the dialog opens (and re-load if the active
  // conversation changed under it). Re-scoping is automatic: a new `key` re-runs this.
  useEffect(() => {
    if (!open || !ready) return;
    let alive = true;
    setLoading(true);
    setError(null);
    const load =
      conversation.kind === "account"
        ? chat.accountHistory(conversation.id, FULL_HISTORY_LIMIT)
        : chat.channelHistory(conversation.id, FULL_HISTORY_LIMIT);
    load
      .then((hist) => {
        if (alive) setItems(hist.map(fromHistoryItem));
      })
      .catch((e) => {
        if (alive) setError(errorMessage(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [open, ready, key, conversation.kind, conversation.id]);

  // Reset the search box each time the dialog opens / re-scopes.
  useEffect(() => {
    if (open) setQuery("");
  }, [open, key]);

  // Partition once; the per-tab arrays are newest-first (history is oldest-first).
  const { all, media, files, links } = useMemo(() => {
    const media: ChatMessage[] = [];
    const files: ChatMessage[] = [];
    const links: ChatMessage[] = [];
    for (const m of items) {
      const cat = categorize(m);
      if (cat === "media") media.push(m);
      else if (cat === "files") files.push(m);
      else if (cat === "links") links.push(m);
    }
    const newestFirst = (a: ChatMessage[]) => [...a].reverse();
    return {
      all: items,
      media: newestFirst(media),
      files: newestFirst(files),
      links: newestFirst(links),
    };
  }, [items]);

  // "All" tab is searchable; the result list filters this conversation's messages.
  const allFiltered = useMemo(
    () => all.filter((m) => matchesQuery(m, query)),
    [all, query],
  );

  const saveFile = async (file: NonNullable<ChatMessage["file"]>) => {
    try {
      const dest = await saveDialog({ defaultPath: file.name });
      if (typeof dest === "string") await chat.saveFile(file.fileConv, dest);
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="conversation-history-trigger"
          title={t("history.trigger")}
          aria-label={t("history.trigger")}
          disabled={!ready}
        >
          <History className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent
        className="max-w-2xl"
        data-testid="conversation-history-dialog"
      >
        <DialogHeader>
          <div className="flex items-start justify-between gap-3 pr-6">
            <div className="min-w-0">
              <DialogTitle>{t("history.title")}</DialogTitle>
              <DialogDescription>
                {t("history.description", { name: conversation.name })}
              </DialogDescription>
            </div>
            {/* Clearing wipes this conversation's local history; close this dialog after. */}
            <ClearHistoryButton withLabel onCleared={() => setOpen(false)} />
          </div>
        </DialogHeader>

        {error && <p className="text-xs text-destructive">{error}</p>}

        <Tabs defaultValue="all" className="w-full">
          <TabsList className="grid w-full grid-cols-4">
            <TabsTrigger value="all" data-testid="history-tab-all">
              {t("history.tabAll")}
              <Badge className="ml-1.5 bg-muted text-muted-foreground">
                {all.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="media" data-testid="history-tab-media">
              {t("history.tabMedia")}
              <Badge className="ml-1.5 bg-muted text-muted-foreground">
                {media.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="files" data-testid="history-tab-files">
              {t("history.tabFiles")}
              <Badge className="ml-1.5 bg-muted text-muted-foreground">
                {files.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="links" data-testid="history-tab-links">
              {t("history.tabLinks")}
              <Badge className="ml-1.5 bg-muted text-muted-foreground">
                {links.length}
              </Badge>
            </TabsTrigger>
          </TabsList>

          {loading && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {t("common.loading")}
            </p>
          )}

          {!loading && (
            <div className="mt-2 max-h-[60vh] overflow-y-auto pr-0.5">
              {/* All — searchable chronological list */}
              <TabsContent value="all" className="space-y-1">
                <div className="relative mb-2">
                  <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    data-testid="history-search-input"
                    placeholder={t("history.searchPlaceholder")}
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    className="pl-9"
                    aria-label={t("history.searchPlaceholder")}
                  />
                </div>
                {all.length === 0 ? (
                  <EmptyTab
                    icon={<History className="h-7 w-7" />}
                    label={t("history.emptyAll")}
                  />
                ) : allFiltered.length === 0 ? (
                  <EmptyTab
                    icon={<SearchX className="h-7 w-7" />}
                    label={t("history.noMatches")}
                  />
                ) : (
                  allFiltered.map((m, i) => (
                    <div
                      key={m.id ?? i}
                      data-testid="history-row"
                      className="rounded-lg px-2.5 py-2 hover:bg-accent/50"
                    >
                      <RowMeta m={m} />
                      <div className="break-words text-sm text-foreground/90">
                        {m.file ? (
                          <span className="inline-flex items-center gap-1.5">
                            {fileGlyph(m.file.name)}
                            <Highlighted text={m.file.name} term={query} />
                          </span>
                        ) : (
                          <Highlighted text={m.text} term={query} />
                        )}
                      </div>
                    </div>
                  ))
                )}
              </TabsContent>

              {/* Media — grid of image/video thumbnails (lazy bytes via MediaPreview) */}
              <TabsContent value="media">
                {media.length === 0 ? (
                  <EmptyTab
                    icon={<ImageIcon className="h-7 w-7" />}
                    label={t("history.emptyMedia")}
                  />
                ) : (
                  <div className="grid grid-cols-3 gap-2 sm:grid-cols-4">
                    {media.map(
                      (m, i) =>
                        m.file && (
                          <div
                            key={m.id ?? i}
                            data-testid="history-media-item"
                            className="aspect-square overflow-hidden rounded-lg border bg-muted/40 [&_img]:h-full [&_img]:rounded-none [&_img]:object-cover [&_video]:h-full"
                            title={m.file.name}
                          >
                            <MediaPreview
                              fileConv={m.file.fileConv}
                              name={m.file.name}
                              size={m.file.size}
                            />
                          </div>
                        ),
                    )}
                  </div>
                )}
              </TabsContent>

              {/* Files — non-media file list with a Save action */}
              <TabsContent value="files" className="space-y-1">
                {files.length === 0 ? (
                  <EmptyTab
                    icon={<FileX className="h-7 w-7" />}
                    label={t("history.emptyFiles")}
                  />
                ) : (
                  files.map(
                    (m, i) =>
                      m.file && (
                        <div
                          key={m.id ?? i}
                          data-testid="history-file-item"
                          className="flex items-center gap-2 rounded-lg px-2.5 py-2 hover:bg-accent/50"
                        >
                          {fileGlyph(m.file.name)}
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-sm">
                              {m.file.name}
                            </div>
                            <div className="flex items-center gap-2 text-xs text-muted-foreground">
                              <span className="font-mono">
                                {humanSize(m.file.size)}
                              </span>
                              <span aria-hidden>·</span>
                              <span>{formatDay(m.wallClock)}</span>
                            </div>
                          </div>
                          <Button
                            size="sm"
                            variant="secondary"
                            className="h-7"
                            onClick={() => void saveFile(m.file!)}
                          >
                            {t("common.save")}
                          </Button>
                        </div>
                      ),
                  )
                )}
              </TabsContent>

              {/* Links — messages containing URLs, linkified (opened in the OS browser) */}
              <TabsContent value="links" className="space-y-1">
                {links.length === 0 ? (
                  <EmptyTab
                    icon={<LinkIcon className="h-7 w-7" />}
                    label={t("history.emptyLinks")}
                  />
                ) : (
                  links.map((m, i) => (
                    <div
                      key={m.id ?? i}
                      data-testid="history-link-item"
                      className="rounded-lg px-2.5 py-2 hover:bg-accent/50"
                    >
                      <RowMeta m={m} />
                      <p className="break-words text-sm text-foreground/90">
                        {renderWithMentions(m.text)}
                      </p>
                    </div>
                  ))
                )}
              </TabsContent>
            </div>
          )}
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
