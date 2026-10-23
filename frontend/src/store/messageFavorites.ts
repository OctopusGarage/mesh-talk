import { notifySyncInvalidation } from "@/lib/syncInvalidation";
import { create } from "zustand";
import {
  messageFavorites,
  type FavoriteMetadata,
  type FavoriteSource,
  type MessageFavorite,
} from "@/lib/api/messageFavorites";
import { errorMessage } from "@/lib/error";
import { captureRuntimeOwner } from "./ownership";

interface FavoriteState {
  items: MessageFavorite[];
  query: string;
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  reset: () => void;
  load: (query: string, append?: boolean, refresh?: boolean) => Promise<void>;
  refresh: () => Promise<void>;
  save: (
    source: FavoriteSource,
    metadata: FavoriteMetadata,
  ) => Promise<MessageFavorite>;
  edit: (
    item: MessageFavorite,
    metadata: FavoriteMetadata,
  ) => Promise<MessageFavorite>;
  remove: (item: MessageFavorite) => Promise<void>;
}
/** The capture dependency makes exact owner/runtime lifetime behavior testable. */
export function createMessageFavoritesStore(
  api: Pick<typeof messageFavorites, "list" | "save" | "edit" | "remove">,
  capture: () => { owner: string | null; current: () => boolean },
) {
  let lifetime = 0;
  let request = 0;
  const empty = {
    items: [] as MessageFavorite[],
    query: "",
    loading: false,
    error: null,
    hasMore: false,
  };
  return create<FavoriteState>((set, get) => {
    const mutate = async <T>(operation: (owner: string) => Promise<T>) => {
      const lease = capture();
      const epoch = lifetime;
      if (!lease.owner || !lease.current())
        throw new Error("Session unavailable");
      const result = await operation(lease.owner);
      if (!lease.current() || epoch !== lifetime)
        throw new Error("Session changed");
      // Invalidate observations started before this durable mutation. The caller
      // owns its editor draft; only a successful result may close that editor.
      request++;
      notifySyncInvalidation({ owner: lease.owner, kind: "state" });
      await get().refresh();
      if (!lease.current() || epoch !== lifetime)
        throw new Error("Session changed");
      return result;
    };
    return {
      ...empty,
      reset: () => {
        lifetime++;
        request++;
        set(empty);
      },
      refresh: () => get().load(get().query, false, true),
      load: async (query, append = false, refresh = false) => {
        const lease = capture();
        if (!lease.owner || !lease.current()) return;
        if (append && (get().loading || get().query !== query)) return;
        const sequence = ++request;
        const epoch = lifetime;
        const offset = append ? get().items.length : 0;
        const wanted = refresh ? Math.max(20, get().items.length) : 20;
        set({
          query,
          loading: true,
          error: null,
          ...(!append && !refresh ? { items: [], hasMore: false } : {}),
        });
        try {
          const accumulated: MessageFavorite[] = [];
          let hasMore = false;
          do {
            const page = await api.list(
              lease.owner,
              query,
              offset + accumulated.length,
              20,
            );
            if (!lease.current() || epoch !== lifetime || sequence !== request)
              return;
            accumulated.push(...page.items);
            hasMore = page.has_more;
            if (!page.items.length) break;
          } while (refresh && hasMore && accumulated.length < wanted);
          const result = { items: accumulated, has_more: hasMore };
          set({
            items: append
              ? [...get().items, ...result.items].filter(
                  (item, index, all) =>
                    all.findIndex((other) => other.id === item.id) === index,
                )
              : result.items,
            hasMore: result.has_more,
            loading: false,
          });
        } catch (e) {
          if (lease.current() && epoch === lifetime && sequence === request)
            set({ error: errorMessage(e), loading: false });
        }
      },
      save: (source, metadata) =>
        mutate((owner) => api.save(owner, source, metadata)),
      edit: (item, metadata) =>
        mutate((owner) =>
          api.edit(owner, item.id, item.revision, item.device_id, metadata),
        ),
      remove: (item) =>
        mutate((owner) =>
          api.remove(owner, item.id, item.revision, item.device_id),
        ),
    };
  });
}
export const useMessageFavorites = createMessageFavoritesStore(
  messageFavorites,
  captureRuntimeOwner,
);
