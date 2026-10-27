import { readWithDeadline } from "@/lib/readDeadline";
import type {
  AccountGroupOperationKind,
  AccountGroupOperationStatus,
} from "@/lib/types";

export interface GroupOperationLease {
  owner: string;
  accountId: string;
  scope: string;
  current(): boolean;
}
export interface GroupOperationTicket {
  id: string;
  account_id: string;
  group: string;
  kind: AccountGroupOperationKind;
  stage: "submitting" | "unconfirmed" | "observed";
  observation: number;
  status?: AccountGroupOperationStatus;
  cancelRequested?: boolean;
  deleteHistory?: boolean;
}
export type GroupOperationOutcome =
  | {
      kind: "committed" | "pending" | "rejected" | "cancelled";
      id: string;
      status: AccountGroupOperationStatus;
    }
  | { kind: "unconfirmed"; id: string; error?: unknown }
  | { kind: "not_admitted"; id?: string; error: unknown }
  | { kind: "capacity" | "stale" };
export type GroupOperationInvoke = (
  owner: string,
  account: string,
  id: string,
) => Promise<AccountGroupOperationStatus>;
export interface GroupOperationContext {
  capture(): GroupOperationLease | null;
  get(id: string): GroupOperationTicket | undefined;
  tickets(): GroupOperationTicket[];
  reserve(ticket: GroupOperationTicket): boolean;
  update(ticket: GroupOperationTicket): void;
  release(id: string): void;
  terminal(status: AccountGroupOperationStatus): void;
  report(
    reason:
      | "invalid_response"
      | "conflicting_terminal"
      | "observation_failed"
      | "terminal_effect_failed"
      | "capacity",
  ): void;
  commands: {
    mint(owner: string, account: string): Promise<string>;
    status(
      owner: string,
      account: string,
      id: string,
    ): Promise<AccountGroupOperationStatus | null>;
    list(
      owner: string,
      account: string,
      query: { pendingOnly: boolean; after?: string; limit: number },
    ): Promise<AccountGroupOperationStatus[]>;
    cancel(
      owner: string,
      account: string,
      id: string,
    ): Promise<AccountGroupOperationStatus>;
  };
}
export interface GroupOperationRequest {
  group: string;
  kind: AccountGroupOperationKind;
  invoke: GroupOperationInvoke;
  deleteHistory?: boolean;
  /** Link payload ownership after reservation, before native mutation. */
  onReserved?: (id: string) => boolean;
}
export function isGroupHistoryCleanupPending(
  status: AccountGroupOperationStatus | undefined,
): boolean {
  return (
    status?.kind === "leave_account" &&
    status.phase.kind === "committed" &&
    status.delete_history === true &&
    status.history_cleanup_complete !== true
  );
}
export function isGroupOperationComplete(
  status: AccountGroupOperationStatus | undefined,
): boolean {
  return (
    !!status &&
    ["committed", "rejected", "cancelled"].includes(status.phase.kind) &&
    !isGroupHistoryCleanupPending(status)
  );
}
const isTerminal = isGroupOperationComplete;
function publicCommitted(status: AccountGroupOperationStatus | undefined) {
  return status?.phase.kind === "committed";
}

/** Leave is one unresolved account action, even when its dialog is recreated. */
export function unresolvedGroupLeave(
  tickets: Iterable<GroupOperationTicket>,
  accountId: string,
  group: string,
): GroupOperationTicket | undefined {
  for (const ticket of tickets) {
    if (
      ticket.account_id === accountId &&
      ticket.group === group &&
      ticket.kind === "leave_account" &&
      !isTerminal(ticket.status)
    )
      return ticket;
  }
}
function outcome(status: AccountGroupOperationStatus): GroupOperationOutcome {
  const phase = status.phase.kind;
  return {
    kind: isGroupHistoryCleanupPending(status)
      ? "pending"
      : phase === "committed" || phase === "rejected" || phase === "cancelled"
        ? phase
        : "pending",
    id: status.id,
    status,
  };
}
function definiteRejection(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "kind" in error &&
    ["authorization", "invalid-input", "not-started"].includes(
      String(error.kind),
    )
  );
}
function sameTerminal(
  left: AccountGroupOperationStatus,
  right: AccountGroupOperationStatus,
): boolean {
  return (
    left.phase.kind === right.phase.kind &&
    ("reason" in left.phase ? left.phase.reason : undefined) ===
      ("reason" in right.phase ? right.phase.reason : undefined) &&
    left.primary_event === right.primary_event &&
    JSON.stringify(left.committed_events) ===
      JSON.stringify(right.committed_events)
  );
}

/** Shared IPC coordination only. The aggregate store owns tickets, message bodies
 * and effects; core owns admission, permission, cancellation and durable recovery. */
export function createGroupOperationCoordinator(
  context: GroupOperationContext,
) {
  let observation = 0;
  const invoking = new Set<string>();
  const invocationKey = (lease: GroupOperationLease, id: string) =>
    `${lease.accountId}:${id}`;
  let scan = {
    scope: "",
    busy: false,
    knownAfter: "",
    after: undefined as string | undefined,
  };

  function ticketFor(lease: GroupOperationLease, id: string) {
    const ticket = context.get(id);
    return ticket?.account_id === lease.accountId ? ticket : undefined;
  }
  function currentOutcome(ticket: GroupOperationTicket): GroupOperationOutcome {
    return ticket.status
      ? outcome(ticket.status)
      : { kind: "unconfirmed", id: ticket.id };
  }
  function apply(
    lease: GroupOperationLease,
    id: string,
    status: AccountGroupOperationStatus,
    token: number,
  ): GroupOperationOutcome {
    if (!lease.current()) return { kind: "stale" };
    const ticket = ticketFor(lease, id);
    if (!ticket) return { kind: "stale" };
    if (
      status.id !== id ||
      status.account_id !== lease.accountId ||
      status.group !== ticket.group ||
      status.kind !== ticket.kind
    ) {
      context.report("invalid_response");
      if (!lease.current()) return { kind: "stale" };
      if (!ticket.status) context.update({ ...ticket, stage: "unconfirmed" });
      return lease.current() ? currentOutcome(ticket) : { kind: "stale" };
    }
    const retainedChoice =
      ticket.deleteHistory ?? ticket.status?.delete_history;
    if (status.kind === "leave_account" && retainedChoice != null) {
      if (
        status.delete_history != null &&
        status.delete_history !== retainedChoice
      ) {
        context.report("conflicting_terminal");
        return currentOutcome(ticket);
      }
      status = { ...status, delete_history: retainedChoice };
    }
    // Public commitment cannot regress while its private cleanup is still pending.
    if (
      publicCommitted(ticket.status) &&
      !sameTerminal(ticket.status!, status)
    ) {
      context.report("conflicting_terminal");
      return currentOutcome(ticket);
    }
    // The public result can arrive before the separate private choice document.
    // Enrich that same immutable result instead of latching unknown as Keep.
    const cleanupMetadataArrived =
      publicCommitted(ticket.status) &&
      status.kind === "leave_account" &&
      ticket.status?.delete_history == null &&
      status.delete_history === true;
    if (isTerminal(ticket.status) && !cleanupMetadataArrived) {
      if (isTerminal(status) && !sameTerminal(ticket.status!, status))
        context.report("conflicting_terminal");
      return lease.current() ? currentOutcome(ticket) : { kind: "stale" };
    }
    if (token < ticket.observation && !isTerminal(status))
      return currentOutcome(ticket);
    context.update({
      ...ticket,
      status,
      stage: "observed",
      observation: token,
    });
    if (!lease.current()) return { kind: "stale" };
    if (isTerminal(status)) {
      try {
        context.terminal(status);
      } catch {
        if (lease.current()) context.report("terminal_effect_failed");
      }
    }
    return lease.current() ? outcome(status) : { kind: "stale" };
  }
  const cancelling = new Set<string>();
  async function requestCancellation(
    lease: GroupOperationLease,
    id: string,
  ): Promise<GroupOperationOutcome> {
    const ticket = ticketFor(lease, id);
    if (!lease.current() || !ticket) return { kind: "stale" };
    if (
      isTerminal(ticket.status) ||
      publicCommitted(ticket.status) ||
      cancelling.has(invocationKey(lease, id))
    )
      return currentOutcome(ticket);
    const key = invocationKey(lease, id);
    cancelling.add(key);
    const token = ++observation;
    try {
      const status = await context.commands.cancel(
        lease.owner,
        lease.accountId,
        id,
      );
      return apply(lease, id, status, token);
    } catch (error) {
      if (!lease.current()) return { kind: "stale" };
      const current = ticketFor(lease, id);
      if (!current) return { kind: "stale" };
      return isTerminal(current.status)
        ? currentOutcome(current)
        : { kind: "unconfirmed", id, error };
    } finally {
      cancelling.delete(key);
    }
  }

  async function invoke(
    lease: GroupOperationLease,
    ticket: GroupOperationTicket,
    send: GroupOperationInvoke,
    firstAttempt: boolean,
  ): Promise<GroupOperationOutcome> {
    const key = invocationKey(lease, ticket.id);
    if (invoking.has(key))
      return currentOutcome(ticketFor(lease, ticket.id) ?? ticket);
    invoking.add(key);
    const token = ++observation;
    try {
      const status = await send(lease.owner, lease.accountId, ticket.id);
      const result = apply(lease, ticket.id, status, token);
      if (
        lease.current() &&
        ticketFor(lease, ticket.id)?.cancelRequested &&
        !isTerminal(ticketFor(lease, ticket.id)?.status)
      )
        return requestCancellation(lease, ticket.id);
      return result;
    } catch (error) {
      if (!lease.current()) return { kind: "stale" };
      const current = ticketFor(lease, ticket.id);
      if (!current) return { kind: "stale" };
      if (current.status) return currentOutcome(current);
      if (firstAttempt && definiteRejection(error)) {
        context.release(ticket.id);
        return lease.current()
          ? { kind: "not_admitted", id: ticket.id, error }
          : { kind: "stale" };
      }
      context.update({ ...current, stage: "unconfirmed" });
      return lease.current()
        ? { kind: "unconfirmed", id: ticket.id, error }
        : { kind: "stale" };
    } finally {
      invoking.delete(key);
    }
  }

  return {
    isInvoking(accountId: string, id: string): boolean {
      return invoking.has(`${accountId}:${id}`);
    },
    async submit(
      request: GroupOperationRequest,
    ): Promise<GroupOperationOutcome> {
      const lease = context.capture();
      if (!lease?.current()) return { kind: "stale" };
      const existingLeave = () =>
        request.kind === "leave_account"
          ? unresolvedGroupLeave(
              context.tickets(),
              lease.accountId,
              request.group,
            )
          : undefined;
      const existing = existingLeave();
      if (existing) return currentOutcome(existing);
      let id: string;
      try {
        id = await context.commands.mint(lease.owner, lease.accountId);
      } catch (error) {
        return lease.current()
          ? { kind: "not_admitted", error }
          : { kind: "stale" };
      }
      if (!lease.current()) return { kind: "stale" };
      // Another submission or recovered native observation can reserve the same
      // logical Leave while minting yields. Keep its original durable identity.
      const recovered = existingLeave();
      if (recovered) return currentOutcome(recovered);
      const ticket: GroupOperationTicket = {
        id,
        account_id: lease.accountId,
        group: request.group,
        kind: request.kind,
        ...(request.deleteHistory === undefined
          ? {}
          : { deleteHistory: request.deleteHistory }),
        stage: "submitting",
        observation: 0,
      };
      const reserved = context.reserve(ticket);
      if (!lease.current()) return { kind: "stale" };
      if (!reserved) return { kind: "capacity" };
      if (request.onReserved && !request.onReserved(id)) {
        if (lease.current()) context.release(id);
        return { kind: "stale" };
      }
      if (!lease.current()) return { kind: "stale" };
      return invoke(lease, ticket, request.invoke, true);
    },
    async resume(
      id: string,
      send: GroupOperationInvoke,
    ): Promise<GroupOperationOutcome> {
      const lease = context.capture();
      if (!lease?.current()) return { kind: "stale" };
      const ticket = ticketFor(lease, id);
      if (!ticket)
        return {
          kind: "not_admitted",
          id,
          error: new Error("Operation is not tracked"),
        };
      if (isTerminal(ticket.status) || publicCommitted(ticket.status))
        return currentOutcome(ticket);
      return invoke(lease, ticket, send, false);
    },
    async cancel(id: string): Promise<GroupOperationOutcome> {
      const lease = context.capture();
      if (!lease?.current()) return { kind: "stale" };
      const ticket = ticketFor(lease, id);
      if (!ticket)
        return {
          kind: "not_admitted",
          id,
          error: new Error("Operation is not tracked"),
        };
      if (isTerminal(ticket.status) || publicCommitted(ticket.status))
        return currentOutcome(ticket);
      context.update({ ...ticket, cancelRequested: true });
      if (!lease.current()) return { kind: "stale" };
      return requestCancellation(lease, id);
    },
    async refresh(): Promise<void> {
      const capturedLease = context.capture();
      if (!capturedLease?.current()) return;
      const lease: GroupOperationLease = capturedLease;
      if (scan.scope !== lease.scope)
        scan = {
          scope: lease.scope,
          busy: false,
          knownAfter: "",
          after: undefined,
        };
      const run = scan;
      if (run.busy) return;
      run.busy = true;
      const known = context
        .tickets()
        .filter(
          (ticket) =>
            ticket.account_id === lease.accountId && !isTerminal(ticket.status),
        )
        .sort((left, right) => left.id.localeCompare(right.id));
      const selected = [
        ...known.filter((ticket) => ticket.id > run.knownAfter),
        ...known.filter((ticket) => ticket.id <= run.knownAfter),
      ].slice(0, 8);
      const pageToken = ++observation;
      async function pendingPage() {
        try {
          const rows = await readWithDeadline(() =>
            context.commands.list(lease.owner, lease.accountId, {
              pendingOnly: true,
              after: run.after,
              limit: 100,
            }),
          );
          if (!lease.current()) return;
          if (
            rows.length > 100 ||
            rows.some(
              (row, index) =>
                row.account_id !== lease.accountId ||
                (index > 0 && row.id <= rows[index - 1].id) ||
                (run.after !== undefined && row.id <= run.after),
            )
          ) {
            context.report("invalid_response");
            run.after = undefined;
            return;
          }
          for (const status of rows) {
            if (!lease.current()) return;
            const existing = context.get(status.id);
            if (existing && existing.account_id !== lease.accountId) {
              context.report("invalid_response");
              continue;
            }
            if (
              !existing &&
              !context.reserve({
                id: status.id,
                account_id: lease.accountId,
                group: status.group,
                kind: status.kind,
                stage: "observed",
                observation: 0,
              })
            ) {
              context.report("capacity");
              continue;
            }
            apply(lease, status.id, status, pageToken);
          }
          run.after =
            rows.length === 100 ? rows[rows.length - 1].id : undefined;
        } catch {
          if (lease.current()) context.report("observation_failed");
        }
      }
      async function knownStatuses() {
        let failed = false;
        for (
          let index = 0;
          index < selected.length && !failed && lease.current();
          index += 2
        ) {
          const pair = selected.slice(index, index + 2);
          run.knownAfter = pair[pair.length - 1].id;
          await Promise.all(
            pair.map(async (ticket) => {
              const token = ++observation;
              try {
                const status = await readWithDeadline(() =>
                  context.commands.status(
                    lease.owner,
                    lease.accountId,
                    ticket.id,
                  ),
                );
                if (status) apply(lease, ticket.id, status, token);
                if (
                  lease.current() &&
                  ticketFor(lease, ticket.id)?.cancelRequested
                )
                  // Bound this observer's wait, while requestCancellation retains
                  // ownership until the original native mutation actually settles.
                  await readWithDeadline(() =>
                    requestCancellation(lease, ticket.id),
                  );
              } catch {
                failed = true;
                if (lease.current()) context.report("observation_failed");
              }
            }),
          );
        }
      }
      try {
        await Promise.all([pendingPage(), knownStatuses()]);
      } finally {
        run.busy = false;
      }
    },
  };
}
