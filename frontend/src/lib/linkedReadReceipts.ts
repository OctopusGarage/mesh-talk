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
  let writing = false;
  let after: AccountDocumentKey | null = null;
  const known = new Set<string>();
  const current = () => active && ports.current();
  const identity = (key: string, id: string) => JSON.stringify([key, id]);
  const flush = async () => {
    if (!current() || !enabled || writing) return;
    writing = true;
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
          budget--;
        }
      }
    } catch {
      /* Retain local seen evidence for the next policy-enabled retry. */
    } finally {
      writing = false;
    }
  };
  const unsubscribe = ports.subscribe(() => {
    void flush();
  });
  return {
    poll: async () => {
      if (!current() || polling) return;
      polling = true;
      try {
        const policy = await readWithDeadline(ports.policy);
        if (!current()) return;
        enabled = policy.history;
        if (!enabled) return;
        const page = await readWithDeadline(() => ports.list(after));
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
        after = page.next;
        // A local mutation retains its serial write lane until native completion,
        // but cannot prevent observing another device's receipts or policy.
        void flush();
      } catch {
        /* A later invalidation or bounded poll retries the same page. */
      } finally {
        polling = false;
      }
    },
    stop: () => {
      active = false;
      enabled = false;
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
    subscribe: (changed) => useChat.subscribe(changed),
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
  const timer = setInterval(() => {
    void sync.poll();
  }, 15000);
  const unsubscribe = subscribeSyncInvalidation((change) => {
    if (change.owner === owner) void sync.poll();
  });
  void sync.poll();
  return () => {
    clearInterval(timer);
    unsubscribe();
    sync.stop();
  };
}
