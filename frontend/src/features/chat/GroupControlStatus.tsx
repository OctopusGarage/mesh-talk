import { errorMessage } from "@/lib/error";
import { useTranslation } from "react-i18next";
import { useChat } from "@/store/chat";
import type { GroupOperationOutcome } from "@/store/accountGroupOperations";
import { Button } from "@/components/ui/button";

export function GroupControlStatus({
  outcome,
  cancelRequested = false,
}: {
  outcome?: GroupOperationOutcome;
  cancelRequested?: boolean;
}) {
  const { t } = useTranslation();
  const cancel = useChat((state) => state.cancelGroupOperation);
  const refresh = useChat((state) => state.refreshGroupOperations);
  const status = outcome && "status" in outcome ? outcome.status : undefined;
  const needsUpgrade = useChat((state) =>
    state.channels?.some(
      (channel) =>
        channel.channel_id === status?.group &&
        channel.account_access?.account_id === state.myAccountId &&
        channel.account_access.execution.kind === "requires_upgrade",
    ),
  );
  if (!outcome || outcome.kind === "stale") return null;
  const waiting = outcome.kind === "pending" || outcome.kind === "unconfirmed";
  const phase = status?.phase.kind;
  const key =
    (cancelRequested && waiting) || phase === "cancellation_requested"
      ? "cancelling"
      : outcome.kind === "unconfirmed"
        ? "unconfirmed"
        : outcome.kind === "committed"
          ? "committed"
          : outcome.kind === "cancelled"
            ? "cancelled"
            : waiting && needsUpgrade
              ? "upgrade"
              : phase === "action_required"
                ? "actionRequired"
                : phase === "waiting_for_executor"
                  ? "waiting"
                  : waiting
                    ? "pending"
                    : "failed";
  return (
    <div
      role={
        waiting || outcome.kind === "committed" || outcome.kind === "cancelled"
          ? "status"
          : "alert"
      }
      className="space-y-2 text-sm text-muted-foreground"
    >
      <p>{t(`groupOperation.${key}`)}</p>
      {outcome.kind === "not_admitted" && <p>{errorMessage(outcome.error)}</p>}
      {status &&
        "reason" in status.phase &&
        status.phase.reason === "owner_must_transfer" && (
          <p>{t("members.transferBeforeLeave")}</p>
        )}
      {waiting && "id" in outcome && outcome.id && (
        <>
          <details className="text-xs">
            <summary className="cursor-pointer">
              {t("groupOperation.details")}
            </summary>
            <code className="mt-2 block break-all">{outcome.id}</code>
          </details>
          <div className="flex flex-wrap gap-2">
            <Button variant="ghost" size="sm" onClick={() => void refresh()}>
              {t("groupOperation.refresh")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={cancelRequested || phase === "cancellation_requested"}
              onClick={() => void cancel(outcome.id!)}
            >
              {t("groupOperation.cancel")}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

/** Recovered controls use the same bounded tickets; no payload or polling belongs here. */
export function PendingGroupControls({
  group,
  excludeId,
}: {
  group: string;
  excludeId?: string;
}) {
  const { t } = useTranslation();
  const account = useChat((state) => state.myAccountId);
  const tickets = useChat((state) => state.groupOperations);
  const labels: Partial<
    Record<import("@/lib/types").AccountGroupOperationKind, string>
  > = {
    file: "linkDevice.attachmentsSync",
    reaction: "groupOperation.reaction",
    recall: "groupOperation.recall",
    rename: "sidebar.renameChannelTitle",
    history_policy: "members.historyTitle",
    dissolve: "members.dissolve",
    remove_account: "members.removeMember",
    transfer_ownership: "members.transferOwner",
    invite: "members.addPeer",
    accept_invitation: "conversation.acceptInvite",
    decline_invitation: "conversation.declineInvite",
    leave_account: "members.leave",
  };
  const terminal = (ticket: NonNullable<typeof tickets>[string]) =>
    ticket.status &&
    ["committed", "cancelled", "rejected"].includes(ticket.status.phase.kind);
  const controls = Object.values(tickets ?? {})
    .filter(
      (ticket) =>
        ticket.id !== excludeId &&
        ticket.account_id === account &&
        ticket.group === group &&
        !["text", "sticker"].includes(ticket.kind) &&
        (ticket.kind !== "file" || !terminal(ticket)),
    )
    .sort(
      (left, right) =>
        Number(!!terminal(left)) - Number(!!terminal(right)) ||
        right.observation - left.observation,
    )
    .slice(0, 8);
  if (!controls.length) return null;
  return (
    <div
      className="space-y-3 border-t pt-3"
      aria-label={t("groupOperation.title")}
    >
      {controls.map((ticket) => {
        const phase = ticket.status?.phase.kind;
        const kind =
          phase === "committed" || phase === "cancelled" || phase === "rejected"
            ? phase
            : "pending";
        return (
          <div key={ticket.id} className="space-y-1">
            <p className="text-xs font-medium">
              {t(labels[ticket.kind] ?? "groupOperation.title")}
            </p>
            <GroupControlStatus
              cancelRequested={ticket.cancelRequested}
              outcome={
                ticket.status
                  ? { kind, id: ticket.id, status: ticket.status }
                  : { kind: "unconfirmed", id: ticket.id }
              }
            />
          </div>
        );
      })}
    </div>
  );
}
