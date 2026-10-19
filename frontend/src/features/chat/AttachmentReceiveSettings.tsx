import { useEffect, useRef, useState } from "react";
import { ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/store/auth";
import { useChat, captureChatOwnership } from "@/store/chat";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";

type Mode = "inherit" | "ask" | "auto";

export function AttachmentReceiveSettings({
  account,
  enabled = true,
}: {
  account?: string;
  enabled?: boolean;
}) {
  const { t } = useTranslation();
  const ownerGeneration = useAuth((state) => state.generation);
  const runEpoch = useChat((state) => state.runEpoch);
  const identityEpoch = useChat((state) => state.identityEpoch);
  const [mode, setMode] = useState<Mode | "">("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [loadFailed, setLoadFailed] = useState(false);
  const view = useRef({ valid: false });
  const savePending = useRef(false);
  useEffect(() => {
    const generation = { valid: true };
    view.current = generation;
    savePending.current = false;
    setMode("");
    if (!enabled) return;
    const lease = captureChatOwnership();
    let current = true;
    setLoading(true);
    setLoadFailed(false);
    setSaving(false);
    setError(null);
    void chat.attachmentReceivePolicy(account).then(
      (policy) => {
        if (!current || !generation.valid || !lease.current()) return;
        setMode(
          account
            ? policy.contact_auto === null
              ? "inherit"
              : policy.contact_auto
                ? "auto"
                : "ask"
            : policy.global_auto
              ? "auto"
              : "ask",
        );
        setLoading(false);
      },
      (reason) => {
        if (!current || !generation.valid || !lease.current()) return;
        setLoadFailed(true);
        setError(errorMessage(reason));
        setLoading(false);
      },
    );
    return () => {
      current = false;
      generation.valid = false;
    };
  }, [account, enabled, ownerGeneration, runEpoch, identityEpoch, reload]);

  const change = async (next: Mode) => {
    if (!enabled || loading || loadFailed || !mode || savePending.current)
      return;
    const generation = view.current;
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    savePending.current = true;
    setSaving(true);
    setError(null);
    try {
      if (account)
        await chat.setAttachmentContactAuto(
          account,
          next === "inherit" ? null : next === "auto",
        );
      else await chat.setAttachmentGlobalAuto(next === "auto");
      if (generation.valid && lease.current()) setMode(next);
    } catch (reason) {
      if (generation.valid && lease.current())
        setError(t("settings.saveFailed", { error: errorMessage(reason) }));
    } finally {
      if (generation.valid && lease.current()) {
        savePending.current = false;
        setSaving(false);
      }
    }
  };

  const control = (
    <div className="space-y-1.5">
      <select
        data-testid={
          account ? "contact-attachment-receive" : "global-attachment-receive"
        }
        value={mode}
        onChange={(event) => void change(event.target.value as Mode)}
        disabled={!enabled || loading || loadFailed || saving || !mode}
        aria-label={
          account ? t("files.contactReceiveMode") : t("files.globalReceiveMode")
        }
        className="h-9 rounded-md border bg-background px-2 text-sm text-foreground"
      >
        {!mode && (
          <option value="">
            {loading ? t("common.loading") : t("files.receiveUnknown")}
          </option>
        )}
        {account && (
          <option value="inherit">{t("files.receiveInherit")}</option>
        )}
        <option value="ask">{t("files.receiveAsk")}</option>
        <option value="auto">{t("files.receiveAuto")}</option>
      </select>
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {loadFailed ? t("files.receiveLoadFailed", { error }) : error}
        </p>
      )}
      {loadFailed && (
        <Button
          variant="secondary"
          size="sm"
          onClick={() => setReload((value) => value + 1)}
        >
          {t("contactVisibility.retry")}
        </Button>
      )}
    </div>
  );

  if (!account)
    return (
      <div
        className="flex items-center justify-between gap-4 py-3"
        data-testid="global-attachment-settings"
      >
        <div className="min-w-0">
          <div className="text-sm font-medium">
            {t("files.globalReceiveMode")}
          </div>
          <p className="text-xs text-muted-foreground">
            {t("files.receiveModeDesc")}
          </p>
        </div>
        {control}
      </div>
    );

  return (
    <Popover
      onOpenChange={(open) => {
        view.current.valid = false;
        if (open) setReload((value) => value + 1);
      }}
    >
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="contact-settings-trigger"
          title={t("files.contactSettings")}
          aria-label={t("files.contactSettings")}
        >
          <ShieldCheck className="h-4 w-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 space-y-3">
        <div>
          <div className="text-sm font-medium">
            {t("files.contactReceiveMode")}
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("files.contactReceiveDesc")}
          </p>
        </div>
        {control}
      </PopoverContent>
    </Popover>
  );
}
