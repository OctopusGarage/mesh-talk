import type {
  GroupOperationContext,
  GroupOperationTicket,
} from "./accountGroupOperations";
export {
  submitGroupControlIntent,
  refreshGroupOperationIntents,
  cancelGroupOperationIntent,
  GROUP_OPERATION_TICKET_CAP,
} from "./outgoingIntent/groupOperations";
import { submitGroupMessageIntent } from "./outgoingIntent/groupOperations";
import type { AccountGroupOperationStatus } from "@/lib/types";
import { readWithDeadline } from "@/lib/readDeadline";
import { chat } from "@/lib/api/chat";
import { errorMessage, sendFailReason } from "@/lib/error";
import { captureChatOwner } from "./ownership";
import { useTransfers } from "./transfers";
import {
  type ConversationCacheState,
  convKey,
  applyProjection,
  completeSendIntentState,
} from "./conversationState";
export {
  reconcileMessages,
  applyProjection,
  fromHistoryItem,
} from "./conversationState";
import type {
  ChatMessage,
  Conversation,
  SendIntent,
  SendPayload,
} from "./conversationModel";
export type { SendIntent, SendPayload } from "./conversationModel";
/** Outgoing work owns intent/cache transitions and serial delivery projection. */
export interface OutgoingIntentState extends ConversationCacheState {
  statusBusy: boolean;
  /** Body-free native operation lifecycle; message payloads remain in intents. */
  groupOperations?: Record<string, GroupOperationTicket>;
  groupOperationIssue?: Parameters<GroupOperationContext["report"]>[0];
}

export interface OutgoingIntentContext extends OutgoingIntentState {
  onGroupOperationTerminal?: (
    status: import("@/lib/types").AccountGroupOperationStatus,
  ) => void;
  expiryNow?: number;
  runEpoch: number;
  identityEpoch: number;
  active: Conversation | null;
  myId: string;
  myAccountId: string;
  reload: () => Promise<boolean>;
  refreshStatuses: (
    conversation?: Conversation,
    invalidate?: boolean,
  ) => Promise<void>;
  setError: (message: string) => void;
}

type Get = () => OutgoingIntentContext;
type Set = (
  partial:
    | Partial<OutgoingIntentState>
    | ((state: OutgoingIntentContext) => Partial<OutgoingIntentState>),
) => void;

export const SEND_INTENT_CAP = 256;

let clientIdCounter = 0;
/** A process-unique id for an optimistic message bubble. */
function nextClientId(): string {
  clientIdCounter += 1;
  return `c${clientIdCounter}`;
}

type StatusRefreshRun = {
  current(): boolean;
  dirty: Map<string, Conversation>;
};
// One lane per aggregate store; old owner/runtime work cannot mark a new run dirty.
const statusRefreshRuns = new WeakMap<Get, StatusRefreshRun>();
const MAX_DIRTY_STATUS_CONVERSATIONS = 50;

export async function refreshDeliveryStatuses(
  rawSet: Set,
  get: Get,
  conversation?: Conversation,
  invalidate = false,
) {
  const lease = captureChatOwner(get);
  const set: Set = (partial) => {
    if (lease.current()) rawSet(partial);
  };
  const c = conversation ?? get().active;
  if (!lease.current() || !c) return;
  const key = convKey(c);
  if (get().statusBusy) {
    const run = statusRefreshRuns.get(get);
    if (invalidate && run?.current()) {
      run.dirty.delete(key);
      run.dirty.set(key, c);
      if (run.dirty.size > MAX_DIRTY_STATUS_CONVERSATIONS)
        run.dirty.delete(run.dirty.keys().next().value!);
    }
    return;
  }
  const run: StatusRefreshRun = { current: lease.current, dirty: new Map() };
  statusRefreshRuns.set(get, run);
  set({ statusBusy: true });
  try {
    if (c.kind === "channel") {
      const ids = [
        ...new Set(
          (get().messages[key] ?? [])
            .filter((m) => m.fromMe && m.id && !m.file)
            .map((m) => m.id!),
        ),
      ];
      const requestedIds = new Set(ids);
      const replicas = new Map<
        string,
        { members: number; postOffices: number }
      >();
      for (let i = 0; i < ids.length; i += 256) {
        const rows = await readWithDeadline(() =>
          chat.channelCustodyStatuses(c.id, ids.slice(i, i + 256)),
        );
        if (!lease.current()) return;
        for (const row of rows)
          replicas.set(row.id, {
            members: row.replicas,
            postOffices: row.post_offices ?? 0,
          });
      }
      set((s) => ({
        messages: s.messages[key]
          ? {
              ...s.messages,
              [key]: s.messages[key].map((m) =>
                m.fromMe && !m.file && m.id && requestedIds.has(m.id)
                  ? {
                      ...m,
                      replicas: replicas.get(m.id)?.members,
                      postOfficeCopies: replicas.get(m.id)?.postOffices,
                    }
                  : m,
              ),
            }
          : s.messages,
      }));
      return;
    }
    // The shared serial ticker also recovers capacity-fallback accepted intents.
    // reload's status refresh sees statusBusy and cannot recursively poll.
    if (
      get().active &&
      convKey(get().active!) === key &&
      Object.values(get().intents).some(
        (i) =>
          convKey(i.conversation) === key &&
          i.acceptanceHistoryOnly &&
          i.message.id,
      )
    ) {
      await get().reload();
      if (!lease.current()) return;
    }
    const ids = [
      ...new Set(
        [
          ...(get().messages[key] ?? []),
          ...Object.values(get().intents)
            .filter((i) => convKey(i.conversation) === key)
            .map((i) => i.message),
        ]
          .filter(
            (m) =>
              m.fromMe &&
              m.id &&
              (m.delivery !== "delivered" ||
                (m.file &&
                  !useTransfers.getState().transfers[m.id]?.delivered)),
          )
          .map((m) => m.id!),
      ),
    ];
    const fileIds = new Set(
      [
        ...(get().messages[key] ?? []),
        ...Object.values(get().intents)
          .filter((i) => convKey(i.conversation) === key)
          .map((i) => i.message),
      ]
        .filter((m) => m.fromMe && m.id && m.file)
        .map((m) => m.id!),
    );
    const projection = new Map<string, "awaiting" | "delivered">();
    const observations = new Map<string, ChatMessage["deliveryObservation"]>();
    for (let i = 0; i < ids.length; i += 256) {
      if (!lease.current()) return;
      const rows = await readWithDeadline(() =>
        chat.deliveryStatuses(lease.owner!, c.id, ids.slice(i, i + 256)),
      );
      if (!lease.current()) return;
      const sampledAt = performance.now();
      for (const row of rows) {
        if (!ids.slice(i, i + 256).includes(row.id)) continue;
        projection.set(row.id, row.status);
        const fact = row.observation;
        if (
          c.id !== get().myAccountId &&
          row.status === "awaiting" &&
          fact &&
          (fact.kind === "attempting" || fact.kind === "retry_scheduled") &&
          Number.isFinite(fact.age_ms) &&
          fact.age_ms >= 0
        ) {
          observations.set(row.id, {
            kind: fact.kind,
            ageMs: fact.age_ms,
            sampledAt,
            ...((fact.reason === "post_office_disabled" ||
              fact.reason === "no_known_route" ||
              fact.reason === "attempt_timed_out") && { reason: fact.reason }),
          });
        }
        if (fileIds.has(row.id)) {
          if (row.file_progress)
            useTransfers
              .getState()
              .applyUploadStatus(row.id, row.file_progress);
          else if (row.status === "delivered")
            // Older status providers cannot report chunk custody. End the
            // placeholder without ever presenting an unverified 100%.
            useTransfers.getState().applyUploadStatus(row.id, {
              done: 0,
              total: 0,
              delivered: true,
            });
        }
      }
    }
    set((s) => ({
      messages: s.messages[key]
        ? {
            ...s.messages,
            [key]: s.messages[key].map((m) =>
              applyProjection(m, projection, observations),
            ),
          }
        : s.messages,
      intents: Object.fromEntries(
        Object.entries(s.intents).map(([id, intent]) => [
          id,
          convKey(intent.conversation) === key
            ? {
                ...intent,
                message: applyProjection(
                  intent.message,
                  projection,
                  observations,
                ),
              }
            : intent,
        ]),
      ),
    }));
  } catch {
    /* A missing projection response is not a delivery claim or send failure. */
  } finally {
    const ownsRun = statusRefreshRuns.get(get) === run;
    if (ownsRun) statusRefreshRuns.delete(get);
    set({ statusBusy: false });
    if (ownsRun) {
      for (const pending of run.dirty.values()) {
        if (!lease.current()) break;
        // Retain this pending hint if a different current read acquired the
        // lane first. Ordinary reload/ticker requests never set this bit.
        await refreshDeliveryStatuses(rawSet, get, pending, true);
      }
    }
  }
}

/** Settle only a committed public message identity; acknowledgement stays pending.
 * Canonical history wins over the optimistic body and original local device. */
export function settleGroupSendIntents(
  set: Set,
  get: Get,
  status: AccountGroupOperationStatus,
): void {
  const lease = captureChatOwner(get);
  if (!lease.current() || get().myAccountId !== status.account_id) return;
  if (!["committed", "rejected", "cancelled"].includes(status.phase.kind))
    return;
  if (status.phase.kind === "committed" && !status.primary_event) return;
  const matches = Object.entries(get().intents).filter(
    ([, intent]) =>
      intent.operationId === status.id &&
      intent.conversation.kind === "channel" &&
      intent.conversation.id === status.group &&
      intent.payload.kind === status.kind,
  );
  for (const [clientId, intent] of matches) {
    if (!lease.current()) return;
    if (intent.message.id) continue;
    const key = convKey(intent.conversation);
    let next: ChatMessage | null;
    if (status.phase.kind === "committed") {
      const canonical = get().messages[key]?.find(
        (message) => message.id === status.primary_event,
      );
      const source = status.executions.find((execution) =>
        execution.committed_events.includes(status.primary_event!),
      );
      if (
        intent.payload.kind === "file" &&
        (!canonical?.file?.fileConv || canonical.metadataPending)
      )
        continue;
      if (!canonical && !source) continue;
      next = {
        ...(canonical ?? { ...intent.message, who: source!.device_id }),
        id: status.primary_event,
        clientId,
        pending: false,
        failed: false,
        failReason: undefined,
      };
    } else if (status.phase.kind === "cancelled") next = null;
    else
      next = {
        ...intent.message,
        pending: false,
        failed: true,
        failReason: "authorization",
      };
    if (!lease.current()) return;
    set((state) =>
      lease.current()
        ? completeSendIntentState(
            state,
            intent.conversation,
            clientId,
            next,
            Math.max(Date.now(), get().expiryNow ?? 0),
          )
        : {},
    );
    // Complete the durable identity before fallible composer cleanup.
    if (status.phase.kind === "committed" && lease.current())
      intent.onAccepted?.();
  }
}

function acknowledgeGroupIntent(set: Set, get: Get, clientId: string): void {
  const lease = captureChatOwner(get);
  const accepted = get().intents[clientId]?.onAccepted;
  if (!lease.current() || !accepted) return;
  set((state) => {
    const current = state.intents[clientId];
    return current
      ? {
          intents: {
            ...state.intents,
            [clientId]: { ...current, onAccepted: undefined },
          },
        }
      : {};
  });
  if (lease.current()) accepted();
}

/** Bounded local intent storage is independent of the 50-conversation cache. */
export async function sendIntent(
  set: Set,
  get: Get,
  c: Conversation,
  payload: SendPayload,
) {
  const dispatch = beginSendIntent(set, get, c, payload);
  if (dispatch) await dispatch;
}

export function beginSendIntent(
  set: Set,
  get: Get,
  c: Conversation,
  payload: SendPayload,
  options: {
    waitForHydration?: boolean;
    onAccepted?: () => void;
    retainFailure?: boolean;
  } = {},
): Promise<boolean> | null {
  const lease = captureChatOwner(get);
  const accountId = get().myAccountId;
  const current = () => lease.current() && get().myAccountId === accountId;
  if (!current()) return null;
  if (Object.keys(get().intents).length >= SEND_INTENT_CAP) {
    get().setError(
      "Too many pending messages. Resolve a failed send before sending more.",
    );
    return null;
  }
  const clientId = nextClientId();
  const message: ChatMessage = {
    id: null,
    clientId,
    fromMe: true,
    who: get().myId,
    text:
      payload.kind === "text"
        ? payload.protection
          ? ""
          : payload.text
        : payload.kind === "sticker"
          ? payload.fallback
          : "",
    replyTo: payload.kind === "text" ? payload.replyTo : null,
    protection: payload.kind !== "sticker" ? payload.protection : null,
    wallClock: Date.now(),
    expiresAtMs: payload.expiresAtMs,
    pending: true,
    sticker: payload.kind === "sticker" ? payload.stickerId : null,
    metadataPending: payload.kind === "file",
    file:
      payload.kind === "file"
        ? {
            name:
              payload.stickerMedia?.name ||
              payload.path.split(/[\\\\/]/).pop() ||
              "File",
            size: payload.stickerMedia?.size ?? 0,
            mime: payload.stickerMedia?.mime ?? "",
            fileConv: "",
            media: payload.media,
          }
        : null,
  };
  const intent = {
    conversation: { ...c },
    payload,
    message,
    onAccepted: options.onAccepted,
  };
  const key = convKey(c);
  set((s) =>
    current()
      ? {
          intents: { ...s.intents, [clientId]: intent },
          messages: {
            ...s.messages,
            [key]: [...(s.messages[key] ?? []), message],
          },
        }
      : {},
  );
  // Store subscribers can replace the owner/account during the optimistic write.
  // Never recapture their replacement lease for this original payload.
  if (!current() || get().intents[clientId] !== intent) {
    set((state) =>
      state.intents[clientId] === intent
        ? completeSendIntentState(state, c, clientId, null)
        : {},
    );
    return null;
  }
  return dispatchIntent(set, get, intent, options);
}

export async function dispatchIntent(
  set: Set,
  get: Get,
  intent: SendIntent,
  {
    waitForHydration = true,
    retainFailure = true,
  }: { waitForHydration?: boolean; retainFailure?: boolean } = {},
): Promise<boolean> {
  const lease = captureChatOwner(get);
  const guarded: Set = (partial) => {
    if (lease.current()) set(partial);
  };
  const { conversation: c, payload, message } = intent;
  const key = convKey(c);
  let admitted = false;
  const clientId = message.clientId!;
  const update = (next: ChatMessage | null) =>
    guarded((state) =>
      completeSendIntentState(
        state,
        c,
        clientId,
        next,
        Math.max(Date.now(), get().expiryNow ?? 0),
      ),
    );
  try {
    if (!lease.current()) return false;
    if (c.kind === "channel") {
      // Payload ownership never moves into tickets. Reuse the stored original on retries.
      const current = get().intents[clientId];
      if (!current) return false;
      const original = current.payload;
      const result = await submitGroupMessageIntent(
        set,
        get,
        {
          group: c.id,
          kind: original.kind,
          onReserved(id) {
            let linked = false;
            guarded((state) => {
              const owned = state.intents[clientId];
              if (!owned || owned.operationId) return {};
              linked = true;
              return {
                intents: {
                  ...state.intents,
                  [clientId]: { ...owned, operationId: id },
                },
              };
            });
            return linked && lease.current();
          },
          invoke: (owner, account, id) =>
            original.kind === "text"
              ? chat.submitAccountGroupText(
                  owner,
                  account,
                  id,
                  c.id,
                  original.text,
                  original.replyTo,
                  original.expiresAtMs,
                  original.protection,
                )
              : original.kind === "file"
                ? chat.submitAccountGroupFile(
                    owner,
                    account,
                    id,
                    c.id,
                    original.path,
                    original.media,
                    clientId,
                    original.expiresAtMs,
                    original.protection,
                  )
                : chat.submitAccountGroupSticker(
                    owner,
                    account,
                    id,
                    c.id,
                    original.stickerId,
                    original.fallback,
                    original.expiresAtMs,
                  ),
        },
        current.operationId,
      );
      if (!lease.current() || result.kind === "stale") return false;
      if (result.kind === "unconfirmed") return true;
      if (result.kind === "capacity")
        throw new Error("Too many pending group operations");
      if (result.kind === "not_admitted") {
        // The coordinator proves that this first attempt could not be admitted.
        if (!current.operationId)
          guarded((state) => {
            const owned = state.intents[clientId];
            return owned
              ? {
                  intents: {
                    ...state.intents,
                    [clientId]: { ...owned, operationId: undefined },
                  },
                }
              : {};
          });
        throw result.error;
      }
      if (result.kind === "pending" || result.kind === "committed") {
        admitted = true;
        acknowledgeGroupIntent(guarded, get, clientId);
      }
      if (!("status" in result)) return false;
      settleGroupSendIntents(guarded, get, result.status);
      if (result.kind === "rejected" || result.kind === "cancelled") {
        if (!retainFailure) update(null);
        return false;
      }
      if (result.kind === "committed") {
        admitted = true;
        const hydration =
          get().active && convKey(get().active!) === key
            ? get().reload()
            : get().refreshStatuses(c);
        if (waitForHydration) {
          await hydration;
          if (lease.current())
            settleGroupSendIntents(guarded, get, result.status);
        } else
          void hydration
            .then(() => {
              if (lease.current())
                settleGroupSendIntents(guarded, get, result.status);
            })
            .catch(() => {});
      }
      return lease.current();
    }
    let accepted: ChatMessage | null = null;
    if (c.kind === "account") {
      const owner = lease.owner!;
      if (payload.kind === "text")
        accepted = {
          ...message,
          id: await chat.enqueueText(
            owner,
            c.id,
            payload.text,
            payload.replyTo,
            payload.expiresAtMs,
            payload.protection,
          ),
          pending: false,
        };
      else if (payload.kind === "sticker")
        accepted = {
          ...message,
          id: await chat.enqueueSticker(
            owner,
            c.id,
            payload.stickerId,
            payload.fallback,
            payload.expiresAtMs,
          ),
          pending: false,
        };
      else {
        const result = await chat.enqueueFile(
          owner,
          c.id,
          payload.path,
          payload.media,
          clientId,
          payload.expiresAtMs,
          payload.protection,
        );
        accepted = {
          ...message,
          id: result.id,
          pending: false,
          file: { ...message.file!, fileConv: result.fileConv },
        };
      }
      admitted = true;
      if (!lease.current()) return false;
      const hadIntent = !!get().intents[clientId];
      update(accepted);
      if (!hadIntent) return true;
    }
    if (!lease.current()) return false;
    if (accepted?.id) {
      intent.onAccepted?.();
      // The durable ID settles admission. Optional UI hydration must not hold the draft.
      const hydration =
        get().active && convKey(get().active!) === key
          ? get().reload()
          : get().refreshStatuses(c);
      if (waitForHydration) await hydration;
      else
        void hydration.catch(() => {
          /* Durable acceptance is not a retryable send failure. */
        });
      return lease.current();
    }
    if (get().active && convKey(get().active!) === key) await get().reload();
    return lease.current();
  } catch (e) {
    if (!lease.current()) return false;
    // Durable acceptance survives hydration failure, even if history already removed the intent.
    if (admitted) return true;
    if (payload.kind === "file") useTransfers.getState().clear(clientId);
    // A durable protected-text composer retains its rejected draft and sole retry ownership.
    if (!retainFailure) {
      update(null);
      return false;
    }
    update({
      ...message,
      pending: false,
      failed: true,
      failReason: sendFailReason(e),
    });
    if (payload.kind === "file" && lease.current())
      get().setError(
        /file manifest exceeds transport frame/i.test(errorMessage(e))
          ? "This attachment has too much metadata to send. Try a shorter name or a smaller folder."
          : `Couldn't send file: ${errorMessage(e)}`,
      );
    return false;
  }
}
