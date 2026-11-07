import { startRecoveryRefresh } from "./recoveryRefresh";
import {
  accountDocuments,
  type AccountDocumentKey,
  type AccountDocumentPage,
  type DocumentConversation,
} from "./api/accountDocuments";
import { chat } from "./api/chat";
import { useChat } from "@/store/chat";
import {
  validAccountSeenIdentity,
  type ReceptionLedger,
} from "@/store/reception";
import { captureRuntimeOwner } from "@/store/ownership";
import { subscribeSyncInvalidation } from "./syncInvalidation";
import { readWithDeadline } from "./readDeadline";

interface ReceiptPorts {
  current: () => boolean;
  ledger: () => ReceptionLedger;
  apply: (key: string, ids: string[]) => void;
  subscribe: (changed: () => void) => () => void;
  policy: () => Promise<{ history: boolean }>;
  list: (after: AccountDocumentKey | null) => Promise<AccountDocumentPage>;
  mark: (conversation: DocumentConversation, id: string) => Promise<unknown>;
}
function conversation(key: string): DocumentConversation | null {
  const [kind, ...parts] = key.split(":");
  const id = parts.join(":");
  if (kind !== "account" && kind !== "channel") return null;
  return id.length === (kind === "account" ? 32 : 64) && /^[0-9a-f]+$/u.test(id)
    ? { kind, id }
    : null;
}

/** Monotone exact reads. Remote application never becomes a local write. */
export function createLinkedReadReceiptSync(ports: ReceiptPorts) {
  let active = true;
  let enabled = false;
  let polling = false;
  let pollDirty = false;
  let writing = false;
  let writeContinuation: ReturnType<typeof setTimeout> | undefined;
  let writeDirty = false;
  let after: AccountDocumentKey | null = null;
  let continuation: ReturnType<typeof setTimeout> | undefined;
  const known = new Set<string>();
  const current = () => active && ports.current();
  const identity = (key: string, id: string) => JSON.stringify([key, id]);
  const flush = async () => {
    if (!current() || !enabled) return;
    if (writing) {
      writeDirty = true;
      return;
    }
    writing = true;
    writeDirty = false;
    if (writeContinuation !== undefined) clearTimeout(writeContinuation);
    writeContinuation = undefined;
    let exhausted = false;
    let failed = false;
    try {
      let budget = 64;
      for (const [key, entries] of Object.entries(ports.ledger())) {
        const target = conversation(key);
        if (!target) continue;
        for (const [id, entry] of Object.entries(entries)) {
          if (!current() || !enabled || !budget) return;
          const token = identity(key, id);
          if (
            !entry.seen ||
            entry.suppressed ||
            known.has(token) ||
            !validAccountSeenIdentity(id)
          )
            continue;
          await ports.mark(target, id);
          if (!current()) return;
          known.add(token);
          ports.apply(key, [id]);
          if (--budget === 0) {
            exhausted = true;
            return;
          }
        }
      }
    } catch {
      failed = true;
      /* Retain local seen evidence for the next policy-enabled retry. */
    } finally {
      writing = false;
      if (!failed && (exhausted || writeDirty) && current() && enabled)
        writeContinuation = setTimeout(() => void flush(), 0);
    }
  };
  const unsubscribe = ports.subscribe(() => {
    void flush();
  });
  const poll = async () => {
    if (!current()) return;
    if (polling) {
      pollDirty = true;
      return;
    }
    polling = true;
    pollDirty = false;
    if (continuation !== undefined) clearTimeout(continuation);
    continuation = undefined;
    let more = false;
    try {
      const policy = await readWithDeadline(ports.policy);
      if (!current()) return;
      enabled = policy.history;
      if (!enabled) return;
      // A low-frequency repair must not make each already-available page wait
      // another five minutes. Drain bounded batches and yield between them.
      for (let budget = 0; budget < 8; budget++) {
        const cursor = after;
        const page = await readWithDeadline(() => ports.list(cursor));
        if (!current() || !enabled) return;
        for (const meta of page.items) {
          if (meta.key.kind !== "read_receipt" || meta.tombstone) continue;
          const { conversation, message_id } = meta.key;
          const key = `${conversation.kind}:${conversation.id}`;
          const token = identity(key, message_id);
          if (known.has(token)) continue;
          known.add(token);
          ports.apply(key, [message_id]);
        }
        if (
          page.next !== null &&
          JSON.stringify(page.next) === JSON.stringify(cursor)
        )
          throw new Error("Receipt cursor did not advance");
        after = page.next;
        if (after === null) break;
      }
      more = after !== null;
      // Local mutations keep their own write lane, independently of observation.
      void flush();
    } catch {
      /* A later invalidation or bounded recovery retries the retained cursor. */
    } finally {
      polling = false;
      if ((more || pollDirty) && current())
        continuation = setTimeout(() => void poll(), 0);
    }
  };
  return {
    poll,
    stop: () => {
      active = false;
      enabled = false;
      if (continuation !== undefined) clearTimeout(continuation);
      if (writeContinuation !== undefined) clearTimeout(writeContinuation);
      unsubscribe();
    },
  };
}

export function startLinkedReadReceipts(owner: string): () => void {
  const lease = captureRuntimeOwner();
  const accountId = useChat.getState().myAccountId;
  const current = () =>
    lease.owner === owner &&
    lease.current() &&
    useChat.getState().ready &&
    useChat.getState().myAccountId === accountId &&
    Boolean(accountId);
  const sync = createLinkedReadReceiptSync({
    current,
    ledger: () => useChat.getState().ledger,
    subscribe: (changed) =>
      useChat.subscribe((state, previous) => {
        if (state.ledger !== previous.ledger) changed();
      }),
    apply: (key, ids) => {
      if (current())
        useChat.getState().applyAccountReadReceipts(owner, accountId, key, ids);
    },
    policy: () => chat.getDeviceSyncPolicy(owner),
    list: (after) =>
      accountDocuments.list(owner, accountId, "read_receipt", after),
    mark: (conversation, id) =>
      accountDocuments.markSeen(owner, accountId, conversation, id),
  });
  const stopRecovery = startRecoveryRefresh(() => void sync.poll());
  const unsubscribe = subscribeSyncInvalidation((change) => {
    if (change.owner === owner) void sync.poll();
  });
  void sync.poll();
  return () => {
    stopRecovery();
    unsubscribe();
    sync.stop();
  };
}
