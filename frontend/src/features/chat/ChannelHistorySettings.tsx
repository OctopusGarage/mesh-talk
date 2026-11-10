import { useChat } from "@/store/chat";
import { useGroupControlOperation } from "./useGroupControlOperation";
import { GroupControlStatus } from "./GroupControlStatus";
import { canManageChannel } from "./channelPermissions";
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
  canManage?: boolean;
  selfAccepted: boolean;
}) {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  // Retire in-flight metadata and saves when the desktop session or membership authority changes.
  return (
    <HistoryPolicyControl
      key={`${props.channelId}:${owner}:${generation}:${props.myId}:${props.channelOwner}:${props.canManage}:${props.selfAccepted}`}
      {...props}
    />
  );
}

function HistoryPolicyControl({
  channelId,
  myId,
  dissolved,
  channelOwner,
  canManage,
  selfAccepted,
}: {
  channelId: string;
  myId: string;
  dissolved: boolean;
  channelOwner: string;
  canManage?: boolean;
  selfAccepted: boolean;
}) {
  const { t } = useTranslation();
  const id = useId();
  const [info, setInfo] = useState<ChannelMembersInfo | null>(null);
  const [error, setError] = useState(false);
  const operation = useGroupControlOperation(channelId);
  const busy = operation.busy;
  const setPolicy = useChat((s) => s.setChannelHistoryPolicy);
  const policyRevision = useChat((s) =>
    Object.values(s.groupOperations ?? {})
      .filter(
        (ticket) =>
          ticket.account_id === s.myAccountId &&
          ticket.group === channelId &&
          ticket.kind === "history_policy" &&
          ticket.status?.phase.kind === "committed",
      )
      .map((ticket) => ticket.id)
      .sort()
      .join(","),
  );
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
  }, [channelId, retry, policyRevision]);
  const self = info?.members.find((m) => m.user_id === myId);
  const editable =
    !dissolved &&
    info?.history_policy_known !== false &&
    (canManage === true ||
      canManageChannel({ owner: channelOwner, can_manage: canManage }, myId)) &&
    selfAccepted &&
    canManageChannel(info, myId) &&
    (info?.account_access !== undefined
      ? info.account_access?.role === "owner"
      : !!self && !self.pending);
  const save = async (policy: ChannelHistoryPolicy) => {
    if (!editable || busy || !info || !policies.includes(policy)) return;
    setError(false);
    await operation.run(
      () => setPolicy(channelId, policy),
      () => {
        if (!lifetime.live) return;
        // Read the committed policy from its owner instead of publishing the draft.
        setRetry((n) => n + 1);
      },
    );
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
      {info?.directory_reconciling && (
        <p role="status" className="text-xs text-muted-foreground">
          {t("members.detailsReconciling")}
        </p>
      )}
      {info ? (
        info.history_policy_known === false ? (
          <div className="space-y-2">
            <p role="status" className="text-sm text-muted-foreground">
              {t("members.historyUnavailable")}
            </p>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setRetry((n) => n + 1)}
            >
              {t("members.historyRetry")}
            </Button>
          </div>
        ) : editable ? (
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
      <GroupControlStatus
        outcome={operation.outcome}
        cancelRequested={operation.cancelRequested}
      />
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
          {info?.history_policy_known !== false && (
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
