import { chat } from "@/lib/api/chat";
import type { HistoryItem } from "@/lib/types";
import { captureChatOwner } from "./ownership";
import {
  emptyReception,
  reconcileHistory,
  receptionPatch,
  type ReceptionState,
} from "./reception";
import {
  type ConversationCacheState,
  emptyConversationCache,
  boundedConversationMap,
  convKey,
  fromHistoryItem,
  reconcileMessages,
} from "./conversationState";
import type { Conversation } from "./conversationModel";

export interface ConversationHistoryState
  extends ConversationCacheState, ReceptionState {
  active: Conversation | null;
  activeRequest: number;
  historyRevision: number | null;
  historyScanBusy: boolean;
  historyScanQueue: Conversation[];
  historyScanRevision: number | null;
  historyBaselines: string[];
  historyFullBaselines: string[];
  loadingRequest: number;
  loading: boolean;
  historyError: string | null;
  unread: Record<string, number>;
}

/** Runtime validity and delivery refresh are supplied by the session composition. */
export interface ConversationHistoryContext extends ConversationHistoryState {
  expiryNow?: number;
  runEpoch: number;
  identityEpoch: number;
  refreshStatuses: () => Promise<void>;
}

type Get = () => ConversationHistoryContext;
type Set = (
  partial:
    | Partial<ConversationHistoryState>
    | ((
        state: ConversationHistoryContext,
      ) => Partial<ConversationHistoryState>),
) => void;

export function emptyConversationHistory(): ConversationHistoryState {
  return {
    ...emptyConversationCache(),
    ...emptyReception(),
    active: null,
    activeRequest: 0,
    historyRevision: null,
    historyScanBusy: false,
    historyScanQueue: [],
    historyScanRevision: null,
    historyBaselines: [],
    historyFullBaselines: [],
    loadingRequest: 0,
    loading: false,
    historyError: null,
    unread: {},
  };
}

type Snapshot =
  | { kind: "presentation"; discoverChanges: boolean; limit: number }
  | { kind: "coverage"; initialized: boolean };

/** A successful snapshot advances coverage together with its receipt decisions. */
function snapshotPatch(
  state: ConversationHistoryState,
  key: string,
  rows: HistoryItem[],
  snapshot: Snapshot,
): Partial<ConversationHistoryState> {
  const next =
    snapshot.kind === "coverage"
      ? reconcileSnapshotCoverage(state, key, rows, snapshot.initialized)
      : reconcileHistory(
          state,
          key,
          rows.map((row) => ({ id: row.id, fromMe: row.from_me })),
          snapshot.discoverChanges &&
            state.historyBaselines.includes(key) &&
            Object.prototype.hasOwnProperty.call(state.ledger, key),
        );
  return {
    ...receptionPatch(state, next),
    historyBaselines: [
      ...state.historyBaselines.filter((k) => k !== key),
      key,
    ].slice(-4096),
    ...(snapshot.kind === "coverage" || snapshot.limit >= 500
      ? {
          historyFullBaselines: [
            ...state.historyFullBaselines.filter((k) => k !== key),
            key,
          ].slice(-4096),
        }
      : {}),
  };
}

const HISTORY_LIMIT = 200;

function historyFor(c: Conversation, owner: string, limit = HISTORY_LIMIT) {
  return c.kind === "account"
    ? chat.ownerHistory(owner, c.id, limit)
    : chat.channelHistory(c.id, limit);
}
function reactionsFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountReactions(c.id)
    : chat.channelReactions(c.id);
}

export async function reloadConversation(
  rawSet: Set,
  get: Get,
  discoverChanges = true,
  stillCurrent: () => boolean = () => true,
) {
  const lease = captureChatOwner(get);
  const current = () => lease.current() && stillCurrent();
  const set: Set = (partial) => {
    if (current()) rawSet(partial);
  };
  if (!current()) return false;
  const c = get().active;
  if (!c) return false;
  const key = convKey(c);
  const loadingRequest = get().loadingRequest + 1;
  const request = loadingRequest;
  set({
    loading: true,
    loadingRequest,
    historyRequests: boundedConversationMap(
      get().historyRequests,
      key,
      request,
    ),
  });
  try {
    const [items, reacts] = await Promise.all([
      historyFor(c, lease.owner!, get().historyLimits[key]),
      reactionsFor(c),
    ]);
    if (
      !current() ||
      get().historyRequests[key] !== request ||
      (!get().cacheOrder.includes(key) &&
        get().active &&
        convKey(get().active!) !== key)
    )
      return false;
    set((s) => {
      const snapshot = snapshotPatch(s, key, items, {
        kind: "presentation",
        discoverChanges,
        limit: s.historyLimits[key] ?? HISTORY_LIMIT,
      });
      const ledger = snapshot.ledger ?? s.ledger;
      return {
        ...snapshot,
        messages: {
          ...s.messages,
          [key]: reconcileMessages(
            items.map((h) => ({
              ...fromHistoryItem(h),
              receivedAt: ledger[key]?.[h.id ?? ""]?.receivedAt,
              arrivalKind: ledger[key]?.[h.id ?? ""]?.arrivalKind,
            })),
            s.messages[key] ?? [],
            Object.values(s.intents).filter(
              (i) => convKey(i.conversation) === key,
            ),
            s.deleted[key] ?? [],
            Math.max(Date.now(), s.expiryNow ?? 0),
          ),
        },
        intents: Object.fromEntries(
          Object.entries(s.intents).filter(
            ([, intent]) =>
              convKey(intent.conversation) !== key ||
              !intent.message.id ||
              (!intent.acceptanceHistoryOnly &&
                !items.some((h) => h.id === intent.message.id)),
          ),
        ),
        reactions: { ...s.reactions, [key]: reacts },
        ...(s.loadingRequest === loadingRequest
          ? { loading: false, historyError: null }
          : {}),
      };
    });
    if (current() && get().active && convKey(get().active!) === key)
      await get().refreshStatuses();
    return current();
  } catch {
    if (
      get().loadingRequest === loadingRequest &&
      get().active &&
      convKey(get().active!) === key
    )
      set({ loading: false, historyError: key });
    return false;
  } finally {
    // Cancellation retires only this request's loading slot; a newer request owns its own.
    if (
      lease.current() &&
      !stillCurrent() &&
      get().loadingRequest === loadingRequest &&
      get().active &&
      convKey(get().active!) === key
    )
      rawSet({ loading: false });
  }
}

export async function revealConversationIdentity(
  rawSet: Set,
  get: Get,
  key: string,
  id: string,
) {
  const lease = captureChatOwner(get);
  if (!lease.current() || !get().active || convKey(get().active!) !== key)
    return false;
  if (get().messages[key]?.some((row) => row.id === id)) return true;
  const request = get().activeRequest;
  rawSet((state) => ({
    historyLimits: boundedConversationMap(state.historyLimits, key, 500),
  }));
  // Expanding an archive is a baseline operation unless an observed import
  // revision already proves that local history is changing.
  const imported =
    get().historyRevision !== null &&
    get().historyScanRevision !== get().historyRevision;
  const loaded = await reloadConversation(rawSet, get, imported);
  return (
    loaded &&
    lease.current() &&
    get().activeRequest === request &&
    get().active !== null &&
    convKey(get().active!) === key &&
    Boolean(get().messages[key]?.some((row) => row.id === id))
  );
}

/** Scan at most eight discovered conversations; retry failures without blocking later work.
 * A roster request can supply additional freshness checks without owning history policy.
 */
export async function scanConversationHistory(
  rawSet: Set,
  get: Get,
  conversations: Conversation[],
  revision: number,
  stillCurrent: () => boolean = () => true,
) {
  const lease = captureChatOwner(get);
  const current = () => lease.current() && stillCurrent();
  const set: Set = (partial) => {
    if (current()) rawSet(partial);
  };
  if (!current() || get().historyScanBusy) return;
  const changed = get().historyRevision !== revision;
  let loaded = true;
  // Keep a bounded rotating work queue: later conversations receive a turn,
  // and an individual failure cannot force earlier snapshots to repeat.
  if (get().historyScanRevision !== revision) {
    set({
      historyScanRevision: revision,
      historyScanQueue: [
        ...get().historyScanQueue,
        ...conversations.filter(
          (c) =>
            !get().historyScanQueue.some(
              (pending) => convKey(pending) === convKey(c),
            ) &&
            (get().historyScanRevision === null ||
              changed ||
              !get().historyBaselines.includes(convKey(c))),
        ),
      ].slice(0, 4096),
    });
  } else if (!get().historyScanQueue.length && !changed) {
    const missing = conversations.filter(
      (c) => !get().historyBaselines.includes(convKey(c)),
    );
    if (missing.length) set({ historyScanQueue: missing.slice(0, 4096) });
  }
  const round = get().historyScanQueue.slice(0, 8);
  if (round.length) {
    set({ historyScanBusy: true });
    try {
      for (const conversation of round) {
        const key = convKey(conversation);
        const initialized = Object.prototype.hasOwnProperty.call(
          get().ledger,
          key,
        );
        try {
          const rows = await historyFor(conversation, lease.owner!, 500);
          if (!current()) return;
          set((state) => ({
            ...snapshotPatch(state, key, rows, {
              kind: "coverage",
              initialized,
            }),
            historyScanQueue: state.historyScanQueue.filter(
              (c) => convKey(c) !== key,
            ),
          }));
        } catch {
          if (!current()) return;
          loaded = false;
          set((state) => ({
            historyScanQueue: [
              ...state.historyScanQueue.filter((c) => convKey(c) !== key),
              conversation,
            ],
          }));
        }
      }
    } finally {
      // This scan owns the busy slot even when its roster request is superseded.
      if (lease.current()) rawSet({ historyScanBusy: false });
    }
  }
  if (!current()) return;
  if (changed && get().active)
    loaded = (await reloadConversation(rawSet, get, true, current)) && loaded;
  if (!current()) return;
  if (loaded && !get().historyScanQueue.length)
    set({ historyRevision: get().historyScanRevision ?? revision });
}

/** Expanded archive coverage is distinct from the latest presentation window. */
function reconcileSnapshotCoverage(
  state: ConversationHistoryState,
  key: string,
  rows: import("@/lib/types").HistoryItem[],
  initialized: boolean,
): ReceptionState {
  const messages = rows.map((row) => ({ id: row.id, fromMe: row.from_me }));
  const known = initialized && state.historyBaselines.includes(key);
  if (known && !state.historyFullBaselines.includes(key)) {
    // Only the comparable latest window has a baseline. Older expansion rows are
    // preexisting archive until a full snapshot establishes local coverage.
    const recent = reconcileHistory(state, key, messages.slice(-200), true);
    return reconcileHistory(recent, key, messages, false);
  }
  return reconcileHistory(state, key, messages, known);
}
