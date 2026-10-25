import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Copy, LoaderCircle, Plug } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { isContactVisible } from "@/lib/contactVisibility";
import { useContactPolicy } from "@/store/contactPolicy";
import { errorMessage } from "@/lib/error";
import {
  localAgentAccess,
  type AgentDestination,
  type LocalAgentAccess,
} from "@/lib/api/localAgentAccess";
import { AgentConversationPicker } from "./AgentConversationPicker";
import { SettingsRow } from "./primitives";
import type { SettingsController } from "./useSettingsController";

/** UI for the existing integration owner; it never starts a separate participant. */
export function LocalAgentSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const policy = useContactPolicy();
  const [access, setAccess] = useState<LocalAgentAccess | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const [loading, setLoading] = useState(false);
  const pending = useRef(false);
  const lifetime = useRef({ active: false });
  useLayoutEffect(() => {
    const current = { active: true };
    lifetime.current = current;
    return () => {
      current.active = false;
    };
  }, [c.identity]);
  const reload = async () => {
    const current = lifetime.current;
    if (!c.owner || !current.active) return;
    setLoading(true);
    setError(null);
    try {
      const next = await localAgentAccess.get(c.owner);
      if (current.active) setAccess(next);
    } catch (cause) {
      if (current.active) setError(errorMessage(cause));
    } finally {
      if (current.active) setLoading(false);
    }
  };
  useEffect(() => {
    // Identity is the session generation, not only the account name.
    const current = lifetime.current;
    if (!c.owner) return;
    const owner = c.owner;
    setAccess(null);
    setError(null);
    setLoading(true);
    void localAgentAccess.get(owner).then(
      (next) => {
        if (current.active) {
          setAccess(next);
          setLoading(false);
        }
      },
      (cause) => {
        if (current.active) {
          setError(errorMessage(cause));
          setLoading(false);
        }
      },
    );
  }, [c.identity, c.owner]);

  const save = async (enabled: boolean, destinations: AgentDestination[]) => {
    const current = lifetime.current;
    const owner = c.owner;
    if (!owner || !access || !current.active || pending.current) return;
    pending.current = true;
    setError(null);
    setCopyState("idle");
    try {
      await c.runSettingAction(
        "integrations",
        async () => {
          try {
            return await localAgentAccess.set(owner, enabled, destinations);
          } catch (cause) {
            // A failed auxiliary file update may follow a successful shutdown.
            // Reload the canonical state rather than claiming access is still live.
            try {
              const next = await localAgentAccess.get(owner);
              if (current.active) setAccess(next);
            } catch {
              /* The original failure remains the actionable error. */
            }
            if (current.active) setError(errorMessage(cause));
            throw cause;
          }
        },
        (next) => {
          if (current.active) setAccess(next);
        },
      );
    } finally {
      pending.current = false;
    }
  };
  const copy = async () => {
    const current = lifetime.current;
    if (!current.active || !access?.running || !access.instructions) return;
    try {
      await navigator.clipboard.writeText(access.instructions);
      if (current.active) setCopyState("copied");
    } catch {
      if (current.active) setCopyState("failed");
    }
  };
  const busy = c.saving || loading;
  // Hidden contacts cannot acquire new grants; existing scopes stay revocable.
  const availableDestinations = (access?.available_destinations ?? []).filter(
    ({ destination }) =>
      access?.destinations.some(
        (selected) =>
          selected.kind === destination.kind && selected.id === destination.id,
      ) ||
      destination.kind === "channel" ||
      isContactVisible(
        destination.id,
        policy.contacts,
        policy.owner === c.owner && policy.loaded,
      ),
  );
  return (
    <section className="min-w-0 space-y-3" data-testid="local-agent-settings">
      <SettingsRow
        id="setting-local-agent-access"
        icon={<Plug className="h-4 w-4" />}
        title={t("agentAccess.title")}
        desc={t("agentAccess.description")}
        reason={
          access && !access.enabled && access.destinations.length === 0
            ? t("agentAccess.selectFirst")
            : undefined
        }
        control={
          <Switch
            id="setting-local-agent-access"
            checked={access?.enabled ?? false}
            disabled={
              busy ||
              !access ||
              (!access.enabled && access.destinations.length === 0)
            }
            onCheckedChange={(enabled) =>
              void save(enabled, access?.destinations ?? [])
            }
          />
        }
      />
      {loading && !access && (
        <p role="status" className="py-2 text-xs text-muted-foreground">
          {t("common.loading")}
        </p>
      )}
      {error && (
        <div role="alert" className="my-2 space-y-2 text-xs text-destructive">
          <p className="break-words">{error}</p>
          <Button
            size="sm"
            variant="outline"
            data-testid="retry-agent-access"
            disabled={busy}
            onClick={() => void reload()}
          >
            {t("contactVisibility.retry")}
          </Button>
        </div>
      )}
      {access && (
        <div className="space-y-3 py-2">
          <p
            className="flex items-center gap-2 text-xs text-muted-foreground"
            role="status"
            aria-live="polite"
            data-testid="agent-access-status"
          >
            {busy && (
              <LoaderCircle
                aria-hidden="true"
                className="h-3.5 w-3.5 shrink-0 animate-spin motion-reduce:animate-none"
              />
            )}
            {t(
              busy
                ? "settings.saving"
                : access.running
                  ? "agentAccess.running"
                  : access.enabled
                    ? "agentAccess.unavailable"
                    : "agentAccess.off",
            )}
          </p>
          {(access.error || (access.enabled && !access.running)) && (
            <div className="space-y-2">
              {access.error && (
                <p
                  role="alert"
                  className="break-words text-xs text-destructive"
                >
                  {access.error}
                </p>
              )}
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => void save(access.enabled, access.destinations)}
              >
                {t("contactVisibility.retry")}
              </Button>
            </div>
          )}
          <AgentConversationPicker
            key={c.identity}
            access={{
              ...access,
              available_destinations: availableDestinations,
            }}
            busy={busy}
            onChange={(destinations) =>
              void save(access.enabled && destinations.length > 0, destinations)
            }
          />
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("agentAccess.scopeHint")}
          </p>
          {access.instructions && (
            <section
              className="space-y-2 border-t pt-4"
              aria-labelledby="agent-setup-heading"
            >
              <h3 id="agent-setup-heading" className="text-[13px] font-medium">
                {t("agentAccess.setupTitle")}
              </h3>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("agentAccess.setupHint")}
              </p>
              <Button
                size="sm"
                variant="outline"
                disabled={!access.running || busy}
                data-testid="copy-agent-instructions"
                onClick={() => void copy()}
              >
                <Copy className="mr-2 h-3.5 w-3.5" />
                {t(
                  copyState === "copied"
                    ? "agentAccess.copied"
                    : "agentAccess.copy",
                )}
              </Button>
              <span className="sr-only" aria-live="polite">
                {copyState === "copied" ? t("agentAccess.copied") : ""}
              </span>
              {copyState === "failed" && (
                <p role="alert" className="text-xs text-destructive">
                  {t("agentAccess.copyFailed")}
                </p>
              )}
              <details
                data-testid="agent-instructions"
                className="min-w-0 text-xs"
                key={access.instructions}
              >
                <summary className="cursor-pointer rounded-sm py-2 font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
                  {t("agentAccess.showInstructions")}
                </summary>
                <pre className="my-2 whitespace-pre-wrap break-words [overflow-wrap:anywhere] rounded-md bg-muted p-3 text-xs leading-relaxed">
                  {access.instructions}
                </pre>
                <p className="break-words text-xs leading-relaxed text-muted-foreground [overflow-wrap:anywhere]">
                  {t("agentAccess.fileHint", {
                    path: access.instructions_path,
                  })}
                </p>
              </details>
            </section>
          )}
        </div>
      )}
    </section>
  );
}
