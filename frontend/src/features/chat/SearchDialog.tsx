import { HighlightedText } from "./HighlightedText";
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
import type { ReferencedSearchHit, RecentSearch } from "@/lib/api/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import { useAuth } from "@/store/auth";
import {
  fromHistoryItem,
  sameSearchReference,
} from "@/store/conversationState";
import { useContactPolicy } from "@/store/contactPolicy";
import { visibleSearchHits } from "@/lib/contactVisibility";
import { rosterProjection } from "@/lib/rosterProjection";

export function SearchDialog() {
  const { t } = useTranslation();
  const open = useChat((s) => s.open);
  const peers = useChat((s) => s.peers);
  const roster = rosterProjection(peers);
  const ready = useChat((s) => s.ready);
  const runEpoch = useChat((s) => s.runEpoch);
  const identityEpoch = useChat((s) => s.identityEpoch);
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setHits] = useState<ReferencedSearchHit[]>([]);
  const contacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const hits = useMemo(
    () => visibleSearchHits(results, peers, contacts, policyLoaded),
    [results, contacts, peers, policyLoaded],
  );
  const [coverage, setCoverage] = useState<RecentSearch["coverage"] | null>(
    null,
  );
  const [contextError, setContextError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const navigation = useRef(0);
  const visibleHits = useRef(hits);
  visibleHits.current = hits;
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState(false);
  const [retryTick, setRetryTick] = useState(0);
  const [activeIdx, setActiveIdx] = useState(0);
  const selectedIdx = Math.min(activeIdx, Math.max(0, hits.length - 1));
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const resultsScroll = useRef<HTMLDivElement>(null);
  const resultRefs = useRef<(HTMLButtonElement | null)[]>([]);

  useEffect(() => {
    if (searching || searchError) return;
    const container = resultsScroll.current;
    const result = resultRefs.current[selectedIdx];
    if (!container || !result) return;
    const containerRect = container.getBoundingClientRect();
    const resultRect = result.getBoundingClientRect();
    if (resultRect.top < containerRect.top) {
      container.scrollTop += resultRect.top - containerRect.top;
    } else if (resultRect.bottom > containerRect.bottom) {
      container.scrollTop += resultRect.bottom - containerRect.bottom;
    }
  }, [selectedIdx, hits, searching, searchError]);

  useEffect(() => {
    // Don't invoke backend search before the node is up (gates the one node-dependent
    // control reachable during the two-phase startup window).
    setHits([]);
    setCoverage(null);
    setContextError(null);
    setOpening(false);
    navigation.current++;
    const lease = captureRuntimeOwner();
    if (!dialogOpen || !ready || !lease.current()) return;
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
        const results = await chat.searchRecent(lease.owner!, query.trim());
        if (active && lease.current()) {
          setHits(results.hits);
          setCoverage(results.coverage);
          setActiveIdx(0);
        }
      } catch {
        if (active && lease.current()) {
          setHits([]);
          setSearchError(true);
        }
      } finally {
        if (active && lease.current()) setSearching(false);
      }
    }, 250);
    return () => {
      active = false;
      clearTimeout(timer.current);
    };
  }, [
    query,
    dialogOpen,
    ready,
    retryTick,
    owner,
    generation,
    runEpoch,
    identityEpoch,
  ]);

  const go = async (h: ReferencedSearchHit) => {
    if (opening || !visibleHits.current.includes(h)) return;
    const lease = captureRuntimeOwner();
    if (!lease.current()) return;
    const request = ++navigation.current;
    const reference = h.reference;
    const account =
      reference.kind === "legacy-device"
        ? (h.account_id ?? roster.accountForDevice(reference.target))
        : reference.target;
    if (!account) {
      setContextError("search.legacyUnavailable");
      return;
    }
    const conv: Conversation = {
      kind: reference.kind === "channel" ? "channel" : "account",
      id: account,
      name: h.label,
    };
    setOpening(true);
    setContextError(null);
    try {
      const context = await chat.searchContext(lease.owner!, reference);
      if (
        !lease.current() ||
        request !== navigation.current ||
        !visibleHits.current.includes(h)
      )
        return;
      if (
        context.status !== "found" ||
        !sameSearchReference(reference, context.reference) ||
        reference.message_id === null ||
        !context.messages.some(
          (row) =>
            row.id === reference.message_id &&
            !row.recalled &&
            (row.expires_at_ms == null || row.expires_at_ms > Date.now()),
        )
      ) {
        setContextError("search.contextUnavailable");
        return;
      }
      void open(conv, {
        reference,
        context: context.messages.map(fromHistoryItem),
      });
      setDialogOpen(false);
    } catch {
      if (lease.current() && request === navigation.current)
        setContextError("search.contextUnavailable");
    } finally {
      if (lease.current() && request === navigation.current) setOpening(false);
    }
  };

  // Keyboard navigation across results (↑/↓ move, Enter opens).
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (searching || opening || searchError || hits.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIdx(Math.min(selectedIdx + 1, hits.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIdx(Math.max(selectedIdx - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const h = hits[selectedIdx];
      if (h) void go(h);
    }
  };

  return (
    <Dialog
      open={dialogOpen}
      onOpenChange={(next) => {
        setDialogOpen(next);
        if (!next) {
          setSearching(false);
          navigation.current++;
        }
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
      <DialogContent
        className="flex max-w-lg flex-col overflow-hidden"
        data-testid="search-dialog"
      >
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
                ? `search-result-${selectedIdx}`
                : undefined
            }
          />
        </div>
        {coverage && !searching && (
          <p role="status" className="text-xs text-muted-foreground">
            {t("search.recentCoverage", {
              count: coverage.per_conversation_limit,
            })}
            {coverage.truncated && <> {t("search.truncated")}</>}
          </p>
        )}
        {contextError && (
          <p role="alert" className="text-sm text-muted-foreground">
            {t(contextError)}
          </p>
        )}
        <div
          ref={resultsScroll}
          className="min-h-0 max-h-80 flex-1 space-y-1 overflow-y-auto"
        >
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
                  disabled={opening}
                  onClick={() => void go(h)}
                  onMouseEnter={() => setActiveIdx(i)}
                  aria-selected={i === selectedIdx}
                  className={
                    "flex w-full items-start gap-3 rounded-lg px-2.5 py-2 text-left transition-colors " +
                    (i === selectedIdx ? "bg-accent" : "hover:bg-accent/50")
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
                      <HighlightedText text={h.text} term={query} />
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
