import {
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { ArrowUpRight, BookOpen, ExternalLink, Search, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { searchHelp } from "../../../../tools/help/search.mjs";
import { renderMarkdown } from "../../../../tools/help/render.mjs";
import { HELP_LANGUAGE, HELP_URL, HELP_VERSION, helpTopics } from "./content";
import "./help.css";

export function HelpDialog({
  open,
  onOpenChange,
  onNavigate,
  trigger,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onNavigate?: (topicId: string) => void;
  trigger?: ReactNode;
}) {
  const { t, i18n } = useTranslation();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("first-run");
  const [linkFailed, setLinkFailed] = useState(false);
  const search = useRef<HTMLInputElement>(null);
  const article = useRef<HTMLElement>(null);
  const results = useRef<HTMLDivElement>(null);
  const navigating = useRef(false);
  const topic = helpTopics.find(({ id }) => id === selected) ?? helpTopics[0];
  const matches = useMemo(() => searchHelp(helpTopics, query), [query]);
  const html = useMemo(() => renderMarkdown(topic.body), [topic]);
  const showArticle = (id: string) => {
    if (!helpTopics.some((entry) => entry.id === id)) return;
    setSelected(id);
    setLinkFailed(false);
    // Also focus when selecting the already-open article.
    article.current?.focus();
    article.current?.scrollTo({ top: 0 });
  };
  useLayoutEffect(() => {
    if (!open) return;
    navigating.current = false;
    setLinkFailed(false);
  }, [open]);
  useLayoutEffect(() => {
    if (open) article.current?.scrollTo({ top: 0 });
  }, [selected, open]);

  const externalLink = (url: string) => {
    if ("__TAURI_INTERNALS__" in window) {
      void openUrl(url).catch(() => setLinkFailed(true));
    } else window.open(url, "_blank", "noopener,noreferrer");
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {trigger && <DialogTrigger asChild>{trigger}</DialogTrigger>}
      <DialogContent
        data-testid="help-dialog"
        className="help-dialog"
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          search.current?.focus();
        }}
        onCloseAutoFocus={(event) => {
          if (navigating.current) event.preventDefault();
        }}
      >
        <DialogHeader className="pr-8">
          <DialogTitle className="flex items-center gap-2">
            <BookOpen className="h-4 w-4 text-muted-foreground" />
            {t("help.title")}
          </DialogTitle>
          <DialogDescription>
            {t("help.description", { version: HELP_VERSION })}
          </DialogDescription>
        </DialogHeader>
        <div className="help-reader">
          <div className="help-index">
            <label htmlFor="help-query" className="sr-only">
              {t("help.search")}
            </label>
            <div className="relative">
              <Search className="pointer-events-none absolute left-3 top-3 h-4 w-4 text-muted-foreground" />
              <input
                id="help-query"
                ref={search}
                type="search"
                data-testid="help-search"
                autoComplete="off"
                value={query}
                placeholder={t("help.search")}
                className="help-search"
                aria-controls="help-results"
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                    event.preventDefault();
                    const buttons =
                      results.current?.querySelectorAll<HTMLButtonElement>(
                        "button",
                      );
                    buttons?.[
                      event.key === "ArrowDown" ? 0 : buttons.length - 1
                    ]?.focus();
                  }
                }}
              />
              {query && (
                <button
                  type="button"
                  className="help-clear"
                  aria-label={t("help.clearSearch")}
                  onClick={() => {
                    setQuery("");
                    search.current?.focus();
                  }}
                >
                  <X className="h-4 w-4" />
                </button>
              )}
            </div>
            <p className="my-3 text-xs text-muted-foreground" role="status">
              {t("help.topicCount", { count: matches.length })}
            </p>
            <div
              id="help-results"
              ref={results}
              className="help-results"
              aria-label={t("help.topics")}
              role="navigation"
            >
              {matches.map((entry) => (
                <button
                  type="button"
                  key={entry.id}
                  data-testid={`help-topic-${entry.id}`}
                  aria-current={selected === entry.id ? "page" : undefined}
                  className="help-result"
                  onClick={() => showArticle(entry.id)}
                  onKeyDown={(event) => {
                    if (event.key !== "ArrowDown" && event.key !== "ArrowUp")
                      return;
                    event.preventDefault();
                    const sibling =
                      event.key === "ArrowDown"
                        ? event.currentTarget.nextElementSibling
                        : event.currentTarget.previousElementSibling;
                    if (sibling instanceof HTMLButtonElement) sibling.focus();
                    else search.current?.focus();
                  }}
                >
                  <span className="block text-[13px] font-medium leading-5">
                    {entry.title}
                  </span>
                  <span className="mt-1 block text-xs leading-5 text-muted-foreground">
                    {entry.summary}
                  </span>
                </button>
              ))}
              {!matches.length && (
                <p
                  data-testid="help-no-results"
                  className="px-1 py-4 text-sm leading-6 text-muted-foreground"
                >
                  {t("help.noResults")}
                </p>
              )}
            </div>
          </div>
          <div className="help-reading-pane">
            {!i18n.language.startsWith(HELP_LANGUAGE) && (
              <p className="help-language-note">{t("help.englishFallback")}</p>
            )}
            <article
              ref={article}
              tabIndex={-1}
              lang="en"
              data-testid="help-article"
              aria-label={topic.title}
              className="help-article [&_h1]:font-display"
              onClick={(event) => {
                if (!(event.target instanceof Element)) return;
                const link = event.target.closest("a");
                if (!link) return;
                const href = link.getAttribute("href") ?? "";
                event.preventDefault();
                const internal = href.match(/^([a-z0-9-]+)\.md$/);
                if (internal) showArticle(internal[1]);
                else if (href.startsWith("https://")) externalLink(href);
              }}
              dangerouslySetInnerHTML={{ __html: html }}
            />
            <div className="help-actions">
              {onNavigate && topic.target && (
                <Button
                  variant="secondary"
                  size="sm"
                  data-testid="help-open-target"
                  onClick={() => {
                    navigating.current = true;
                    onNavigate(topic.id);
                  }}
                >
                  <ArrowUpRight className="h-4 w-4" />
                  {topic.target.kind === "settings"
                    ? t("help.openSettings", {
                        category: t(
                          `settings.categories.${topic.target.category}`,
                        ),
                      })
                    : t("help.openDiagnostics")}
                </Button>
              )}
              <Button
                variant="ghost"
                size="sm"
                onClick={() => externalLink(`${HELP_URL}${topic.id}.html`)}
              >
                <ExternalLink className="h-3.5 w-3.5" />
                {t("help.online")}
              </Button>
              {linkFailed && (
                <p role="alert" className="text-xs text-destructive">
                  {t("help.linkFailed")}
                </p>
              )}
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
