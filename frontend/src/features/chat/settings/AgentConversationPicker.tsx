import { useState } from "react";
import { ChevronDown, Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type {
  AgentDestination,
  LocalAgentAccess,
} from "@/lib/api/localAgentAccess";

const key = (destination: AgentDestination) =>
  `${destination.kind}:${destination.id}`;

/** Filtering is presentation only; the caller persists the complete explicit scope. */
export function AgentConversationPicker({
  access,
  busy,
  onChange,
}: {
  access: Pick<LocalAgentAccess, "destinations" | "available_destinations">;
  busy: boolean;
  onChange: (destinations: AgentDestination[]) => void;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [selectedOnly, setSelectedOnly] = useState(false);
  const selected = new Set(access.destinations.map(key));
  const options = [...(access.available_destinations ?? [])];
  for (const destination of access.destinations ?? []) {
    if (!options.some((item) => key(item.destination) === key(destination)))
      options.push({
        destination,
        label: t("agentAccess.unavailableConversation", { id: destination.id }),
      });
  }
  const search = query.trim().toLocaleLowerCase();
  const visibleOptions = options.filter(
    ({ destination, label }) =>
      (!selectedOnly || selected.has(key(destination))) &&
      label.toLocaleLowerCase().includes(search),
  );
  return (
    <details
      className="group min-w-0 rounded-md border"
      data-testid="agent-conversation-picker"
    >
      <summary className="flex min-h-11 cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 rounded-md px-3 py-2 text-xs outline-none hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <span className="flex-1 font-medium">
          {t("agentAccess.conversations")}
        </span>
        <span className="text-muted-foreground" aria-live="polite">
          {t("agentAccess.selectedCount", {
            count: selected.size,
            total: options.length,
          })}
        </span>
        <ChevronDown
          aria-hidden="true"
          className="h-3.5 w-3.5 shrink-0 text-muted-foreground group-open:rotate-180"
        />
      </summary>
      <div className="space-y-2 border-t p-2">
        <div className="relative">
          <Search
            aria-hidden="true"
            className="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            aria-label={t("agentAccess.search")}
            placeholder={t("agentAccess.search")}
            className="pl-9"
          />
        </div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-pressed={selectedOnly}
          data-testid="agent-selected-only"
          className={
            selectedOnly ? "bg-accent text-foreground" : "text-muted-foreground"
          }
          onClick={() => setSelectedOnly((value) => !value)}
        >
          {t("agentAccess.selectedOnly")}
        </Button>
        <fieldset disabled={busy} className="min-w-0">
          <legend className="sr-only">{t("agentAccess.conversations")}</legend>
          <div
            className="max-h-56 overflow-y-auto overscroll-contain"
            data-testid="agent-conversation-list"
          >
            {visibleOptions.map(({ destination, label }) => (
              <label
                key={key(destination)}
                className="flex min-h-10 min-w-0 cursor-pointer items-center gap-2 rounded-sm px-2 py-2 text-xs hover:bg-accent/50 focus-within:bg-accent/50 has-[:disabled]:cursor-default has-[:disabled]:opacity-50"
              >
                <input
                  type="checkbox"
                  className="h-3.5 w-3.5 shrink-0 accent-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                  checked={selected.has(key(destination))}
                  disabled={
                    !selected.has(key(destination)) && selected.size >= 16
                  }
                  onChange={(event) => {
                    const next = event.target.checked
                      ? [...access.destinations, destination]
                      : access.destinations.filter(
                          (item) => key(item) !== key(destination),
                        );
                    onChange(next);
                  }}
                />
                <span className="min-w-0 flex-1 truncate" title={label}>
                  {label}
                </span>
                <span className="shrink-0 text-muted-foreground">
                  {t(
                    destination.kind === "channel"
                      ? "agentAccess.group"
                      : "agentAccess.person",
                  )}
                </span>
              </label>
            ))}
            {visibleOptions.length === 0 && (
              <p className="px-2 py-5 text-center text-xs text-muted-foreground">
                {t(
                  options.length === 0
                    ? "agentAccess.noConversations"
                    : "agentAccess.noMatches",
                )}
              </p>
            )}
          </div>
        </fieldset>
        {selected.size >= 16 && (
          <p className="px-2 text-xs text-muted-foreground" aria-live="polite">
            {t("agentAccess.limitReached")}
          </p>
        )}
      </div>
    </details>
  );
}
