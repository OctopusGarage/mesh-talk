import { readWithDeadline } from "@/lib/readDeadline";
import type {
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMembersInfo,
} from "@/lib/types";
import type { Conversation } from "./conversationModel";

const FALLBACK_MS = 30_000;
const MEMBER_CONCURRENCY = 4;
const MAX_DIRTY_CHANNELS = 256;

export interface ChannelRefreshState {
  active: Conversation | null;
  activeRequest: number;
  channels: ChannelInfo[];
  members: ChannelMemberInfo[];
  channelOwner: string;
  channelMembersById: Record<string, ChannelMemberInfo[]>;
}
interface Context {
  read(): ChannelRefreshState;
  write(
    patch: (state: ChannelRefreshState) => Partial<ChannelRefreshState>,
  ): void;
  capture(): { current(): boolean };
  commands: {
    listChannels(): Promise<ChannelInfo[]>;
    channelMembers(id: string): Promise<ChannelMembersInfo>;
  };
  now?: () => number;
}

function unchanged<T>(before: T, after: T): T {
  return JSON.stringify(before) === JSON.stringify(after) ? before : after;
}

/** Coalesced group invalidations. A full fallback also repairs missed events and
 * older hosts; failed queries preserve the last successful member projection. */
export function createChannelRefresh(context: Context) {
  const now = context.now ?? (() => performance.now());
  type Run = {
    current(): boolean;
    dirty: Set<string>;
    all: boolean;
    promise?: Promise<void>;
    nextFallback: number;
    fullInFlight: boolean;
    publication: number;
  };
  let run: Run | undefined;

  function currentRun() {
    if (!run?.current()) {
      run = {
        ...context.capture(),
        dirty: new Set(),
        all: false,
        nextFallback: 0,
        fullInFlight: false,
        publication: 0,
      };
    }
    return run;
  }

  async function drain(work: Run) {
    while (work.current() && (work.all || work.dirty.size)) {
      const publication = work.publication;
      const all = work.all;
      work.fullInFlight = all;
      const dirty = work.dirty;
      work.all = false;
      work.dirty = new Set();
      const activeRequest = context.read().activeRequest;
      let publicationTimer: ReturnType<typeof setTimeout> | undefined;
      try {
        const channels = await readWithDeadline(() =>
          context.commands.listChannels(),
        );
        if (!work.current()) return;
        if (publication !== work.publication) continue;
        const previous = context.read();
        const wanted = channels.filter(
          (channel) =>
            all ||
            dirty.has(channel.channel_id) ||
            !previous.channelMembersById[channel.channel_id],
        );
        const members = new Map<string, ChannelMembersInfo>();
        const publish = () => {
          if (!work.current() || publication !== work.publication) return;
          context.write((state) => {
            const byId: Record<string, ChannelMemberInfo[]> = {};
            for (const channel of channels) {
              const id = channel.channel_id;
              const fresh = members.get(id)?.members;
              const old = state.channelMembersById[id];
              if (fresh) byId[id] = old ? unchanged(old, fresh) : fresh;
              else if (old) byId[id] = old;
            }
            const patch: Partial<ChannelRefreshState> = {
              channels: unchanged(state.channels, channels),
              channelMembersById: unchanged(state.channelMembersById, byId),
            };
            // Member/owner state belongs to the navigation request that initiated
            // this refresh. Avatar caches may still update after navigation.
            if (
              state.activeRequest === activeRequest &&
              state.active?.kind === "channel"
            ) {
              const id = state.active.id;
              const info = members.get(id);
              const summary = channels.find(
                (channel) => channel.channel_id === id,
              );
              if (summary && state.active.name !== summary.name)
                patch.active = { ...state.active, name: summary.name };
              if (info) {
                patch.members = byId[id];
                patch.channelOwner = info.owner;
              }
            }
            return patch;
          });
        };
        publish();
        let failed = false;
        let index = 0;
        await Promise.all(
          Array.from(
            { length: Math.min(MEMBER_CONCURRENCY, wanted.length) },
            async () => {
              while (
                work.current() &&
                publication === work.publication &&
                index < wanted.length
              ) {
                const id = wanted[index++].channel_id;
                try {
                  members.set(
                    id,
                    await readWithDeadline(() =>
                      context.commands.channelMembers(id),
                    ),
                  );
                  if (work.current() && publicationTimer === undefined) {
                    publicationTimer = setTimeout(() => {
                      publicationTimer = undefined;
                      if (work.current()) publish();
                    }, 50);
                  }
                } catch {
                  failed = true;
                }
              }
            },
          ),
        );
        if (!work.current()) return;
        if (publicationTimer !== undefined) {
          clearTimeout(publicationTimer);
          publicationTimer = undefined;
        }
        publish();
        if (all && !failed) work.nextFallback = now() + FALLBACK_MS;
        // A failed targeted fetch must also retry through anti-entropy.
        if (failed)
          work.nextFallback = Math.min(work.nextFallback, now() + 2000);
      } catch {
        // Leave known summaries and members intact; the next fallback retries.
        work.nextFallback = Math.min(work.nextFallback, now() + 2000);
      } finally {
        if (publicationTimer !== undefined) clearTimeout(publicationTimer);
        work.fullInFlight = false;
      }
    }
  }

  function request(channelId: string | null = null): Promise<void> {
    const work = currentRun();
    if (!work.current()) return Promise.resolve();
    if (channelId === null || work.dirty.size >= MAX_DIRTY_CHANNELS) {
      work.all = true;
      work.dirty.clear();
    } else if (!work.all) work.dirty.add(channelId);
    if (!work.promise) start(work);
    return work.promise!;
  }

  function start(work: Run): Promise<void> {
    // One microtask joins synchronous notification bursts into one batch.
    const promise = Promise.resolve()
      .then(() => drain(work))
      .finally(() => {
        work.promise = undefined;
        // An invalidation may arrive after drain resolves but before this
        // continuation runs. Keep its work and callers inside the same chain.
        if (work.current() && (work.all || work.dirty.size)) return start(work);
      });
    work.promise = promise;
    return promise;
  }

  return {
    request,
    /** Personal deletion supersedes any older list/member response in this lane. */
    invalidate(): Promise<void> {
      const work = currentRun();
      work.publication++;
      return request();
    },
    ifDue(): Promise<void> {
      const work = currentRun();
      if (!work.current() || now() < work.nextFallback)
        return work.promise ?? Promise.resolve();
      // A targeted refresh cannot postpone repair of missed changes in other
      // groups. Join a running full sweep or queue one bounded follow-up.
      if (work.promise) {
        if (!work.fullInFlight) work.all = true;
        return work.promise;
      }
      return request();
    },
  };
}
