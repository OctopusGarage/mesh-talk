import { useEffect, useState } from "react";
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
  const [mode, setMode] = useState<Mode>("ask");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!enabled) return;
    const lease = captureChatOwnership();
    let current = true;
    setLoading(true);
    setSaving(false);
    setError(null);
    void chat.attachmentReceivePolicy(account).then(
      (policy) => {
        if (!current || !lease.current()) return;
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
        if (!current || !lease.current()) return;
        setError(errorMessage(reason));
        setLoading(false);
      },
    );
    return () => {
      current = false;
    };
  }, [account, enabled, ownerGeneration, runEpoch, identityEpoch]);

  const change = async (next: Mode) => {
    if (saving) return;
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    setSaving(true);
    setError(null);
    try {
      if (account)
        await chat.setAttachmentContactAuto(
          account,
          next === "inherit" ? null : next === "auto",
        );
      else await chat.setAttachmentGlobalAuto(next === "auto");
      if (lease.current()) setMode(next);
    } catch (reason) {
      if (lease.current()) setError(errorMessage(reason));
    } finally {
      if (lease.current()) setSaving(false);
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
        disabled={loading || saving}
        aria-label={
          account ? t("files.contactReceiveMode") : t("files.globalReceiveMode")
        }
        className="h-9 rounded-md border bg-background px-2 text-sm text-foreground"
      >
        {account && (
          <option value="inherit">{t("files.receiveInherit")}</option>
        )}
        <option value="ask">{t("files.receiveAsk")}</option>
        <option value="auto">{t("files.receiveAuto")}</option>
      </select>
      {error && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
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
    <Popover>
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
