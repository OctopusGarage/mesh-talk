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
}

export interface OutgoingIntentContext extends OutgoingIntentState {
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
                m.id && replicas.has(m.id)
                  ? {
                      ...m,
                      replicas: replicas.get(m.id)!.members,
                      postOfficeCopies: replicas.get(m.id)!.postOffices,
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
  options: { waitForHydration?: boolean } = {},
): Promise<boolean> | null {
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
            name: payload.path.split(/[\\\\/]/).pop() || "File",
            size: 0,
            mime: "",
            fileConv: "",
            media: payload.media,
          }
        : null,
  };
  const intent = { conversation: { ...c }, payload, message };
  const key = convKey(c);
  set((s) => ({
    intents: { ...s.intents, [clientId]: intent },
    messages: { ...s.messages, [key]: [...(s.messages[key] ?? []), message] },
  }));
  return dispatchIntent(set, get, intent, options);
}

export async function dispatchIntent(
  set: Set,
  get: Get,
  intent: SendIntent,
  { waitForHydration = true }: { waitForHydration?: boolean } = {},
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
    } else {
      const owner = lease.owner!;
      if (payload.kind === "text")
        accepted = {
          ...message,
          id: await chat.enqueueChannelText(
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
          id: await chat.enqueueChannelSticker(
            owner,
            c.id,
            payload.stickerId,
            payload.fallback,
            payload.expiresAtMs,
          ),
          pending: false,
        };
      else {
        const result = await chat.enqueueChannelFile(
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
