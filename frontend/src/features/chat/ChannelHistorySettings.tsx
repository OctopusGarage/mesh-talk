import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { chat } from "@/lib/api";
import type { ChannelHistoryPolicy, ChannelMembersInfo } from "@/lib/types";
import { useAuth } from "@/store/auth";

const policies: ChannelHistoryPolicy[] = [
  "off",
  "last_ten",
  "last_three_days",
  "last_ten_within_three_days",
];

export function ChannelHistorySettings(props: {
  channelId: string;
  myId: string;
  dissolved: boolean;
  channelOwner: string;
  selfAccepted: boolean;
}) {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  // Retire in-flight metadata and saves when the desktop session or membership authority changes.
  return (
    <HistoryPolicyControl
      key={`${props.channelId}:${owner}:${generation}:${props.myId}:${props.channelOwner}:${props.selfAccepted}`}
      {...props}
    />
  );
}

function HistoryPolicyControl({
  channelId,
  myId,
  dissolved,
  channelOwner,
  selfAccepted,
}: {
  channelId: string;
  myId: string;
  dissolved: boolean;
  channelOwner: string;
  selfAccepted: boolean;
}) {
  const { t } = useTranslation();
  const id = useId();
  const [info, setInfo] = useState<ChannelMembersInfo | null>(null);
  const [error, setError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [retry, setRetry] = useState(0);
  const [lifetime] = useState(() => ({ live: true }));
  useEffect(() => {
    lifetime.live = true;
    return () => {
      lifetime.live = false;
    };
  }, [lifetime]);
  useEffect(() => {
    let live = true;
    setError(false);
    void chat.channelMembers(channelId).then(
      (value) => {
        if (live) setInfo(value);
      },
      () => {
        if (live) setError(true);
      },
    );
    return () => {
      live = false;
    };
  }, [channelId, retry]);
  const self = info?.members.find((m) => m.user_id === myId);
  const editable =
    !dissolved &&
    channelOwner === myId &&
    selfAccepted &&
    info?.owner === myId &&
    !!self &&
    !self.pending;
  const save = async (policy: ChannelHistoryPolicy) => {
    if (!editable || busy || !info || !policies.includes(policy)) return;
    setBusy(true);
    setError(false);
    try {
      await chat.setChannelHistoryPolicy(channelId, policy);
      if (lifetime.live) setInfo({ ...info, history_policy: policy });
    } catch {
      if (lifetime.live) setError(true);
    } finally {
      if (lifetime.live) setBusy(false);
    }
  };
  return (
    <div className="space-y-2 border-t border-border pt-3">
      <label
        htmlFor={editable ? id : undefined}
        className="block text-sm font-medium"
      >
        {t("members.historyTitle")}
      </label>
      <p className="text-xs text-muted-foreground">
        {t("members.historyHelp")}
      </p>
      {info ? (
        editable ? (
          <select
            id={id}
            value={info.history_policy}
            disabled={busy}
            onChange={(e) => void save(e.target.value as ChannelHistoryPolicy)}
            className="min-h-11 w-full rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
          >
            {policies.map((policy) => (
              <option key={policy} value={policy}>
                {t(`members.historyPolicy.${policy}`)}
              </option>
            ))}
          </select>
        ) : (
          <p className="text-sm">
            {t(`members.historyPolicy.${info.history_policy}`)}
          </p>
        )
      ) : (
        !error && (
          <p role="status" className="text-sm text-muted-foreground">
            {t("members.historyLoading")}
          </p>
        )
      )}
      {busy && (
        <p role="status" className="text-xs text-muted-foreground">
          {t("members.historySaving")}
        </p>
      )}
      {error && (
        <div>
          <p role="alert" className="text-sm text-destructive">
            {t("members.historyError")}
          </p>
          {!info && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setRetry((n) => n + 1)}
            >
              {t("members.historyRetry")}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
