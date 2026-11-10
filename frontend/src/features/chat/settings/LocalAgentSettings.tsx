import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Copy, LoaderCircle, Plug } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { isContactVisible } from "@/lib/contactVisibility";
import { useContactPolicy } from "@/store/contactPolicy";
import { errorMessage, isVerificationCanceled } from "@/lib/error";
import {
  localAgentAccess,
  type AgentDestination,
  type AgentAccessChange,
  type AgentAccessLifetime,
  type LocalAgentAccess,
} from "@/lib/api/localAgentAccess";
import { AgentConversationPicker } from "./AgentConversationPicker";
import { SettingsRow, SELECT_CLASS } from "./primitives";
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
  const [duration, setDuration] = useState<AgentAccessLifetime>("one_hour");
  const [clock, setClock] = useState(Date.now);
  const reads = useRef(0);
  const reading = useRef<object | null>(null);
  const pending = useRef(false);
  const lifetime = useRef({ active: false });
  useLayoutEffect(() => {
    const current = { active: true };
    lifetime.current = current;
    pending.current = false;
    reading.current = null;
    setAccess(null);
    return () => {
      current.active = false;
    };
  }, [c.identity, c.owner]);
  const reload = async (background = false) => {
    const current = lifetime.current;
    if (!c.owner || !current.active) return;
    if (pending.current || reading.current) return;
    const request = {};
    reading.current = request;
    const sequence = ++reads.current;
    if (!background) setLoading(true);
    setError(null);
    try {
      const next = await localAgentAccess.get(c.owner);
      if (current.active && sequence === reads.current && !pending.current)
        setAccess(next);
    } catch (cause) {
      if (current.active && sequence === reads.current)
        setError(errorMessage(cause));
    } finally {
      if (reading.current === request) reading.current = null;
      if (current.active && sequence === reads.current) setLoading(false);
    }
  };
  useEffect(() => {
    // Identity is the session generation, not only the account name.
    const current = lifetime.current;
    if (!c.owner) return;
    const owner = c.owner;
    const sequence = ++reads.current;
    const request = {};
    reading.current = request;
    setCopyState("idle");
    setAccess(null);
    setError(null);
    setLoading(true);
    void localAgentAccess.get(owner).then(
      (next) => {
        if (reading.current === request) reading.current = null;
        if (current.active && sequence === reads.current) {
          setAccess(next);
          setDuration(next.enabled ? next.lifetime : "one_hour");
          setLoading(false);
        }
      },
      (cause) => {
        if (reading.current === request) reading.current = null;
        if (current.active && sequence === reads.current) {
          setError(errorMessage(cause));
          setLoading(false);
        }
      },
    );
  }, [c.identity, c.owner]);

  const save = async (
    enabled: boolean,
    destinations: AgentDestination[],
    change?: AgentAccessChange,
  ) => {
    const current = lifetime.current;
    const owner = c.owner;
    if (!owner || !access || !current.active || pending.current) return;
    pending.current = true;
    ++reads.current;
    setError(null);
    setCopyState("idle");
    try {
      await c.runSettingAction(
        "integrations",
        async () => {
          try {
            return await (change
              ? localAgentAccess.set(owner, enabled, destinations, change)
              : localAgentAccess.set(owner, enabled, destinations));
          } catch (cause) {
            if (isVerificationCanceled(cause)) throw cause;
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
      if (current.active) pending.current = false;
    }
  };
  const expired =
    !!access?.enabled &&
    (access.expired ||
      (access.expires_at_ms !== null && access.expires_at_ms <= clock));
  useEffect(() => {
    if (!access?.enabled || access.expired || access.expires_at_ms === null)
      return;
    const remaining = access.expires_at_ms - Date.now();
    if (remaining <= 0) {
      setClock(Date.now());
      return;
    }
    const timer = window.setTimeout(
      () => {
        setClock(Date.now());
        void reload(true);
      },
      Math.min(remaining, 2_147_483_647),
    );
    return () => window.clearTimeout(timer);
    // Refresh only at the acknowledged deadline; reads never renew a grant.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    access?.enabled,
    access?.expired,
    access?.expires_at_ms,
    c.identity,
    c.owner,
  ]);
  const copy = async () => {
    const current = lifetime.current;
    if (!current.active || expired || !access?.running || !access.instructions)
      return;
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
              void save(
                enabled,
                access?.destinations ?? [],
                enabled && !access?.enabled
                  ? { lifetime: duration }
                  : undefined,
              )
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
                : expired
                  ? "agentAccess.expired"
                  : access.running
                    ? "agentAccess.running"
                    : access.enabled
                      ? "agentAccess.unavailable"
                      : "agentAccess.off",
            )}
          </p>
          {!expired &&
            (access.error || (access.enabled && !access.running)) && (
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
          <div className="space-y-2 text-xs">
            <label
              htmlFor="agent-access-duration"
              className="block font-medium"
            >
              {t("agentAccess.duration")}
            </label>
            <select
              id="agent-access-duration"
              className={SELECT_CLASS}
              value={duration}
              disabled={busy}
              onChange={(event) => {
                const next = event.target.value as AgentAccessLifetime;
                setDuration(next);
              }}
            >
              <option value="one_hour">{t("agentAccess.oneHour")}</option>
              <option value="persistent">{t("agentAccess.persistent")}</option>
            </select>
            {access.enabled && (
              <p className="text-muted-foreground">
                {access.lifetime === "persistent" ? (
                  t("agentAccess.persistent")
                ) : access.expires_at_ms !== null &&
                  Number.isFinite(access.expires_at_ms) ? (
                  <>
                    {t(
                      expired
                        ? "agentAccess.expiredAt"
                        : "agentAccess.expiresAt",
                    )}{" "}
                    <time
                      dateTime={new Date(access.expires_at_ms).toISOString()}
                    >
                      {new Date(access.expires_at_ms).toLocaleString()}
                    </time>
                  </>
                ) : (
                  t("agentAccess.expired")
                )}
              </p>
            )}
            {access.enabled && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy || !access.destinations.length}
                data-testid="renew-agent-access"
                onClick={() =>
                  void save(true, access.destinations, {
                    lifetime: duration,
                    renew: true,
                  })
                }
              >
                {t("agentAccess.renew")}
              </Button>
            )}
            <p className="text-muted-foreground">
              {t("agentAccess.durationHint")}
            </p>
          </div>
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
          <div
            data-testid="agent-access-activity"
            className="space-y-2 border-t pt-3 text-xs"
          >
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 className="font-medium">{t("agentAccess.activityTitle")}</h3>
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                data-testid="refresh-agent-activity"
                onClick={() => void reload()}
              >
                {t("agentAccess.refreshActivity")}
              </Button>
            </div>
            <p className="text-muted-foreground">
              {t("agentAccess.activityDeliveryHint")}
            </p>
            {access.activity_error ? (
              <p role="alert" className="text-destructive">
                {t("agentAccess.activityFailed")}
              </p>
            ) : (access.recent_activity ?? []).length === 0 ? (
              <p className="text-muted-foreground">
                {t("agentAccess.activityEmpty")}
              </p>
            ) : (
              <ul className="max-h-40 space-y-2 overflow-y-auto">
                {(access.recent_activity ?? []).map((item, index) => (
                  <li
                    key={`${item.at_ms}:${index}`}
                    className="flex flex-wrap gap-x-2 gap-y-1"
                  >
                    <time
                      className="text-muted-foreground"
                      dateTime={new Date(item.at_ms).toISOString()}
                    >
                      {new Date(item.at_ms).toLocaleString()}
                    </time>
                    <span className="min-w-0 break-words [overflow-wrap:anywhere]">
                      {access.available_destinations.find(
                        (option) =>
                          option.destination.kind === item.destination.kind &&
                          option.destination.id === item.destination.id,
                      )?.label ??
                        t("agentAccess.unavailableConversation", {
                          id: item.destination.id,
                        })}
                    </span>
                    <span>
                      {t(
                        item.action === "retried"
                          ? "agentAccess.activityRetried"
                          : "agentAccess.activityAccepted",
                      )}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          {access.instructions && !expired && (
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
                disabled={expired || !access.running || busy}
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
