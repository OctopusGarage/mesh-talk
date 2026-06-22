import { useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";
import { Hash, Search, SearchX } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IdentityGlyph } from "@/components/identity";
import { chat } from "@/lib/api";
import { formatDay } from "@/lib/format";
import { fadeSlideUp, listStagger, useMotionOK } from "@/lib/motion";
import { useChat, type Conversation } from "@/store/chat";
import type { SearchHitInfo } from "@/lib/types";

/** Highlight every case-insensitive occurrence of `term` in `text` with the signal hue. */
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

export function SearchDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const open = useChat((s) => s.open);
  const peers = useChat((s) => s.peers);
  const ready = useChat((s) => s.ready);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHitInfo[]>([]);
  const [searching, setSearching] = useState(false);
  const [activeIdx, setActiveIdx] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    // Don't invoke backend search before the node is up (gates the one node-dependent
    // control reachable during the two-phase startup window).
    if (!dialogOpen || !ready) return;
    if (!query.trim()) {
      setHits([]);
      return;
    }
    setSearching(true);
    clearTimeout(timer.current);
    // `active` guards against a stale resolution: if the query changes (or the dialog
    // closes) while a search is in flight, its result must not overwrite the newer one.
    let active = true;
    timer.current = setTimeout(async () => {
      try {
        const results = await chat.search(query.trim());
        if (active) {
          setHits(results);
          setActiveIdx(0);
        }
      } catch {
        if (active) setHits([]);
      } finally {
        if (active) setSearching(false);
      }
    }, 250);
    return () => {
      active = false;
      clearTimeout(timer.current);
    };
  }, [query, dialogOpen, ready]);

  const go = (h: SearchHitInfo) => {
    let conv: Conversation;
    if (h.is_channel) {
      conv = { kind: "channel", id: h.target, name: h.label };
    } else {
      // Resolve a device-id target to its account so account_history works.
      const peer = peers.find((p) => p.user_id === h.target);
      conv = {
        kind: "account",
        id: peer?.account_id ?? h.target,
        name: h.label,
      };
    }
    void open(conv);
    setDialogOpen(false);
  };

  // Keyboard navigation across results (↑/↓ move, Enter opens).
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (hits.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIdx((i) => Math.min(i + 1, hits.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIdx((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const h = hits[activeIdx];
      if (h) go(h);
    }
  };

  return (
    <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-action-search"
          title={t("search.title")}
          disabled={!ready}
        >
          <Search className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-lg" data-testid="search-dialog">
        <DialogHeader>
          <DialogTitle>{t("search.title")}</DialogTitle>
        </DialogHeader>
        <div className="relative">
          <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            autoFocus
            data-testid="search-input"
            placeholder={t("search.placeholder")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            className="pl-9"
            aria-label={t("search.title")}
          />
        </div>
        <div className="max-h-80 space-y-1 overflow-y-auto">
          {searching && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {t("search.searching")}
            </p>
          )}
          {!searching && query.trim() && hits.length === 0 && (
            <div className="flex flex-col items-center gap-2 py-8 text-center">
              <SearchX className="h-7 w-7 text-muted-foreground/60" />
              <p className="text-sm text-muted-foreground">
                {t("search.noMatches")}
              </p>
            </div>
          )}
          {!searching && !query.trim() && (
            <div className="flex flex-col items-center gap-2 py-8 text-center">
              <Search className="h-7 w-7 text-muted-foreground/40" />
              <p className="text-sm text-muted-foreground">
                {t("search.empty")}
              </p>
            </div>
          )}
          <motion.div
            initial={motionOK ? "hidden" : false}
            animate="visible"
            variants={listStagger}
          >
            {hits.map((h, i) => (
              <motion.button
                key={i}
                variants={fadeSlideUp}
                data-testid="search-result"
                onClick={() => go(h)}
                onMouseEnter={() => setActiveIdx(i)}
                aria-selected={i === activeIdx}
                className={
                  "flex w-full items-start gap-3 rounded-lg px-2.5 py-2 text-left transition-colors " +
                  (i === activeIdx ? "bg-accent" : "hover:bg-accent/50")
                }
              >
                {h.is_channel ? (
                  <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[28%] border border-border bg-secondary text-muted-foreground">
                    <Hash className="h-4 w-4" />
                  </div>
                ) : (
                  <IdentityGlyph
                    seed={h.target}
                    size={36}
                    className="shrink-0"
                  />
                )}
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate font-display text-sm font-semibold tracking-tight">
                      {h.is_channel ? "#" : ""}
                      {h.label}
                    </span>
                    <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                      {formatDay(h.wall_clock)}
                    </span>
                  </div>
                  <div className="truncate text-sm text-muted-foreground">
                    <span className="text-foreground/70">
                      {h.from_me ? t("common.you") : h.who}:
                    </span>{" "}
                    <Highlighted text={h.text} term={query} />
                  </div>
                </div>
              </motion.button>
            ))}
          </motion.div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
