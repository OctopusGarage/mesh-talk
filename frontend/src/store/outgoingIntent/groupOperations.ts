import { chat } from "@/lib/api/chat";
import type {
  AccountGroupControlRequest,
  AccountGroupOperationKind,
} from "@/lib/types";
import { captureChatOwner } from "../ownership";
import {
  createGroupOperationCoordinator,
  isGroupOperationComplete,
  type GroupOperationOutcome,
  type GroupOperationRequest,
} from "../accountGroupOperations";
import type {
  OutgoingIntentContext,
  OutgoingIntentState,
} from "../outgoingIntent";

type Get = () => OutgoingIntentContext;
type Set = (
  patch:
    | Partial<OutgoingIntentState>
    | ((state: OutgoingIntentContext) => Partial<OutgoingIntentState>),
) => void;
/** Bounded UI metadata; the native journal retains durable operation identity. */
export const GROUP_OPERATION_TICKET_CAP = 256;
const coordinators = new WeakMap<
  Get,
  ReturnType<typeof createGroupOperationCoordinator>
>();

function coordinator(set: Set, get: Get) {
  const known = coordinators.get(get);
  if (known) return known;
  const current = createGroupOperationCoordinator({
    capture() {
      const owner = captureChatOwner(get);
      const state = get();
      if (!owner.owner || !state.myAccountId) return null;
      const accountId = state.myAccountId;
      return {
        owner: owner.owner,
        accountId,
        scope: JSON.stringify([
          owner.owner,
          owner.generation,
          owner.run,
          state.identityEpoch,
          accountId,
        ]),
        current: () => owner.current() && get().myAccountId === accountId,
      };
    },
    get: (id) => get().groupOperations?.[id],
    tickets: () => Object.values(get().groupOperations ?? {}),
    reserve(ticket) {
      let reserved = false;
      set((state) => {
        if (state.myAccountId !== ticket.account_id) return {};
        const entries = Object.entries(state.groupOperations ?? {}).filter(
          ([, entry]) => entry.account_id === ticket.account_id,
        );
        if (entries.some(([id]) => id === ticket.id)) return {};
        if (entries.length >= GROUP_OPERATION_TICKET_CAP) {
          const index = entries.findIndex(
            ([id, entry]) =>
              entry.status &&
              isGroupOperationComplete(entry.status) &&
              !current.isInvoking(entry.account_id, id) &&
              !Object.values(state.intents).some(
                (intent) => intent.operationId === id,
              ),
          );
          if (index < 0) return {};
          entries.splice(index, 1);
        }
        reserved = true;
        return {
          groupOperations: {
            ...Object.fromEntries(entries),
            [ticket.id]: ticket,
          },
          groupOperationIssue: undefined,
        };
      });
      return reserved;
    },
    update(ticket) {
      set((state) =>
        state.myAccountId === ticket.account_id
          ? {
              groupOperations: {
                ...state.groupOperations,
                [ticket.id]: ticket,
              },
              groupOperationIssue: undefined,
            }
          : {},
      );
    },
    release(id) {
      set((state) => {
        const groupOperations = { ...state.groupOperations };
        delete groupOperations[id];
        return { groupOperations };
      });
    },
    terminal: (status) => get().onGroupOperationTerminal?.(status),
    report: (reason) => set({ groupOperationIssue: reason }),
    commands: {
      mint: chat.newAccountGroupOperationId,
      status: chat.accountGroupOperationStatus,
      list: chat.listAccountGroupOperations,
      cancel: chat.cancelAccountGroupOperation,
    },
  });
  coordinators.set(get, current);
  return current;
}

function controlKind(
  operation: AccountGroupControlRequest,
): AccountGroupOperationKind {
  return operation.kind === "react"
    ? "reaction"
    : operation.kind === "set_history_policy"
      ? "history_policy"
      : operation.kind;
}
export async function submitGroupControlIntent(
  set: Set,
  get: Get,
  group: string,
  operation: AccountGroupControlRequest,
): Promise<GroupOperationOutcome> {
  return coordinator(set, get).submit({
    group,
    kind: controlKind(operation),
    ...(operation.kind === "leave_account" &&
    operation.delete_history !== undefined
      ? { deleteHistory: operation.delete_history }
      : {}),
    invoke: (owner, account, id) =>
      chat.submitAccountGroupControl(owner, account, id, group, operation),
  });
}
export async function refreshGroupOperationIntents(
  set: Set,
  get: Get,
): Promise<void> {
  await coordinator(set, get).refresh();
}
export async function cancelGroupOperationIntent(
  set: Set,
  get: Get,
  id: string,
): Promise<GroupOperationOutcome> {
  return coordinator(set, get).cancel(id);
}

/** The existing bounded coordinator owns all group mutation observation. */
export async function submitGroupMessageIntent(
  set: Set,
  get: Get,
  request: GroupOperationRequest,
  existingId?: string,
): Promise<GroupOperationOutcome> {
  const operations = coordinator(set, get);
  return existingId
    ? operations.resume(existingId, request.invoke)
    : operations.submit(request);
}
