import { useEffect, useMemo, useRef, useState } from "react";
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
import { useChat, type Conversation } from "@/store/chat";
import type { SearchHitInfo } from "@/lib/types";
import { useContactPolicy } from "@/store/contactPolicy";
import { visibleSearchHits } from "@/lib/contactVisibility";

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
  const open = useChat((s) => s.open);
  const peers = useChat((s) => s.peers);
  const ready = useChat((s) => s.ready);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setHits] = useState<SearchHitInfo[]>([]);
  const contacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const hits = useMemo(
    () => visibleSearchHits(results, peers, contacts, policyLoaded),
    [results, contacts, peers, policyLoaded],
  );
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState(false);
  const [retryTick, setRetryTick] = useState(0);
  const [activeIdx, setActiveIdx] = useState(0);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const resultRefs = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    if (!searching && !searchError)
      resultRefs.current[activeIdx]?.scrollIntoView({ block: "nearest" });
  }, [activeIdx, hits, searching, searchError]);

  useEffect(() => {
    // Don't invoke backend search before the node is up (gates the one node-dependent
    // control reachable during the two-phase startup window).
    if (!dialogOpen || !ready) return;
    if (!query.trim()) {
      setHits([]);
      setSearching(false);
      setSearchError(false);
      return;
    }
    setSearching(true);
    setSearchError(false);
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
        if (active) {
          setHits([]);
          setSearchError(true);
        }
      } finally {
        if (active) setSearching(false);
      }
    }, 250);
    return () => {
      active = false;
      clearTimeout(timer.current);
    };
  }, [query, dialogOpen, ready, retryTick]);

  const go = (h: SearchHitInfo) => {
    let conv: Conversation;
    if (h.is_channel) {
      conv = { kind: "channel", id: h.target, name: h.label };
    } else {
      // Resolve a device-id target to its account so account_history works.
      const peer = peers.find((p) => p.user_id === h.target);
      conv = {
        kind: "account",
        id: h.account_id ?? peer?.account_id ?? h.target,
        name: h.label,
      };
    }
    void open(conv, {
      wallClock: h.wall_clock,
      text: h.text,
      fromMe: h.from_me,
    });
    setDialogOpen(false);
  };

  // Keyboard navigation across results (↑/↓ move, Enter opens).
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (searching || searchError || hits.length === 0) return;
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
    <Dialog
      open={dialogOpen}
      onOpenChange={(next) => {
        setDialogOpen(next);
        if (!next) setSearching(false);
      }}
    >
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          data-testid="sidebar-action-search"
          title={t("search.title")}
          aria-label={t("search.title")}
          disabled={!ready}
          className="h-9 w-full min-w-0 justify-start gap-2 rounded-md border border-input bg-background/45 px-2.5 text-[12px] font-normal text-muted-foreground hover:border-ring/40 hover:bg-background/70 hover:text-foreground"
        >
          <Search className="h-4 w-4 shrink-0" />
          <span className="truncate">{t("search.title")}</span>
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
            onChange={(e) => {
              setQuery(e.target.value);
              setHits([]);
              setActiveIdx(0);
              setSearchError(false);
            }}
            onKeyDown={onKeyDown}
            className="pl-9"
            aria-label={t("search.title")}
            role="combobox"
            aria-controls="search-results"
            aria-expanded={!searching && !searchError && hits.length > 0}
            aria-activedescendant={
              !searching && !searchError && hits.length > 0
                ? `search-result-${activeIdx}`
                : undefined
            }
          />
        </div>
        <div className="max-h-80 space-y-1 overflow-y-auto">
          {searching && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {t("search.searching")}
            </p>
          )}
          {!searching && searchError && (
            <div
              role="alert"
              className="flex flex-col items-center gap-2 py-8 text-center text-sm text-muted-foreground"
            >
              <p>{t("search.failed")}</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setRetryTick((n) => n + 1)}
              >
                {t("common.retry")}
              </Button>
            </div>
          )}
          {!searching && !searchError && query.trim() && hits.length === 0 && (
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
          <div id="search-results" role="listbox">
            {!searching &&
              !searchError &&
              hits.map((h, i) => (
                <button
                  key={i}
                  ref={(node) => {
                    resultRefs.current[i] = node;
                  }}
                  id={`search-result-${i}`}
                  role="option"
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
                </button>
              ))}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
