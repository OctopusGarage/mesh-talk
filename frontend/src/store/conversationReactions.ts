import { chat } from "@/lib/api/chat";
import type { ReactionInfo, ReactionSummaryPage } from "@/lib/types";
import type { Conversation } from "./conversationModel";
import { convKey } from "./conversationState";
import { captureChatOwner } from "./ownership";

interface State {
  active: Conversation | null;
  runEpoch: number;
  identityEpoch: number;
  activeRequest: number;
}
type Get = () => State;
type Patch = {
  reactions: Record<string, ReactionInfo[]>;
  reactionRemainder: Record<string, string[]>;
};
type Request = {
  current: () => boolean;
  owner: string;
  conversation: Conversation;
  targets: string[];
  set: (patch: Patch) => void;
};
type Lane = { queued: Request | null };
const lanes = new WeakMap<Get, Lane>();
const CACHE_ROWS = 512;
const CACHE_BYTES = 2 * 1024 * 1024;

/** Bound every native response before retaining it. Six bytes per UTF-16 unit
 * covers JSON escaping without allocating a second serialized emoji string. */
export function reactionPageBytes(page: ReactionSummaryPage): number {
  if (
    !page ||
    !Array.isArray(page.items) ||
    page.items.length > 128 ||
    typeof page.pending !== "boolean" ||
    (page.next !== null &&
      (typeof page.next !== "string" || page.next.length > 4096))
  )
    throw new Error("Invalid reaction page");
  if (page.pending && (page.items.length !== 0 || page.next === null))
    throw new Error("Invalid reaction continuation");
  let bytes = 256 + (page.next?.length ?? 0) * 6;
  for (const row of page.items) {
    if (
      typeof row.target !== "string" ||
      row.target.length !== 64 ||
      typeof row.emoji !== "string" ||
      !Number.isSafeInteger(row.count) ||
      row.count < 0 ||
      typeof row.from_me !== "boolean"
    )
      throw new Error("Invalid reaction summary");
    bytes += row.emoji.length * 6 + 256;
  }
  if (bytes > 1024 * 1024 + 8192)
    throw new Error("Reaction page exceeds byte budget");
  return bytes;
}

/** One actual native read and one coalesced latest demand, independent of the
 * history/read ledger. Pending projection turns yield without growing a queue. */
export function requestConversationReactions(
  set: (patch: Patch) => void,
  get: Get,
  conversation: Conversation,
  visible: string[],
) {
  const lease = captureChatOwner(get);
  const key = convKey(conversation);
  const activeRequest = get().activeRequest;
  const current = () =>
    lease.current() &&
    get().activeRequest === activeRequest &&
    get().active !== null &&
    convKey(get().active!) === key;
  if (!current() || !lease.owner) return;
  const request: Request = {
    set,
    current,
    owner: lease.owner,
    conversation,
    targets: [...new Set(visible)].slice(-500),
  };
  const running = lanes.get(get);
  if (running) {
    running.queued = request;
    return;
  }
  const lane: Lane = { queued: request };
  lanes.set(get, lane);
  void drain(get, lane);
}

async function drain(get: Get, lane: Lane) {
  try {
    while (lane.queued) {
      const request = lane.queued;
      lane.queued = null;
      const key = convKey(request.conversation);
      if (request.current())
        request.set({
          reactions: { [key]: [] },
          reactionRemainder: { [key]: request.targets },
        });
      const items: ReactionInfo[] = [];
      const remainder = new Set<string>();
      let bytes = 0;
      for (let offset = 0; offset < request.targets.length; offset += 64) {
        if (!request.current() || lane.queued) break;
        const targets = request.targets.slice(offset, offset + 64);
        if (items.length >= CACHE_ROWS || bytes >= CACHE_BYTES) {
          request.targets
            .slice(offset)
            .forEach((target) => remainder.add(target));
          break;
        }
        let cursor: string | null = null;
        try {
          while (request.current() && !lane.queued) {
            const page = await readReactionPage(
              request.owner,
              request.conversation,
              targets,
              cursor,
            );
            if (!request.current() || lane.queued) break;
            reactionPageBytes(page);
            if (page.pending) {
              cursor = page.next;
              await new Promise<void>((resolve) => setTimeout(resolve, 0));
              continue;
            }
            if (page.next) targets.forEach((target) => remainder.add(target));
            for (const row of page.items) {
              if (!targets.includes(row.target))
                throw new Error("Reaction target changed");
              const cost = row.emoji.length * 6 + 256;
              if (items.length >= CACHE_ROWS || bytes + cost > CACHE_BYTES) {
                targets.forEach((target) => remainder.add(target));
                break;
              }
              items.push(row);
              bytes += cost;
            }
            break;
          }
        } catch {
          targets.forEach((target) => remainder.add(target));
        }
      }
      if (request.current() && !lane.queued)
        request.set({
          reactions: { [key]: items },
          reactionRemainder: { [key]: [...remainder] },
        });
    }
  } finally {
    lanes.delete(get);
  }
}

interface PageRead {
  owner: string;
  conversation: Conversation;
  targets: string[];
  cursor: string | null;
  foreground: boolean;
  resolve: (page: ReactionSummaryPage) => void;
  reject: (error: unknown) => void;
}
let reading = false;
let waiting: PageRead | null = null;

/** The message inspector and inline summaries share physical IPC admission.
 * Closing a view cannot release a native read that is still running. */
export function readReactionPage(
  owner: string,
  conversation: Conversation,
  targets: string[],
  cursor: string | null,
  foreground = false,
): Promise<ReactionSummaryPage> {
  return new Promise((resolve, reject) => {
    const request = {
      owner,
      conversation,
      targets,
      cursor,
      foreground,
      resolve,
      reject,
    };
    if (!reading) {
      reading = true;
      void readPage(request);
      return;
    }
    const replaced = new Error("Reaction request superseded");
    if (waiting?.foreground && !foreground) {
      reject(replaced);
      return;
    }
    waiting?.reject(replaced);
    waiting = request;
  });
}
async function readPage(request: PageRead) {
  try {
    const page = await chat.reactionSummaries(
      request.owner,
      request.conversation.kind,
      request.conversation.id,
      request.targets,
      request.cursor,
    );
    reactionPageBytes(page);
    if (page.items.some((row) => !request.targets.includes(row.target)))
      throw new Error("Reaction target changed");
    request.resolve(page);
  } catch (error) {
    request.reject(error);
  } finally {
    const next = waiting;
    waiting = null;
    if (next) void readPage(next);
    else reading = false;
  }
}
