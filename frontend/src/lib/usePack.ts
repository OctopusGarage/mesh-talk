import { useCallback, useEffect, useState } from "react";
import { usePacks } from "@/store/packs";
import { stickerById, stickerPackId } from "./stickerPacks";
import type { CustomizationPack } from "./pack";

/** Component-owned payloads disappear when the request or its authority retires. */
function usePackProjection<T>(
  id: string | undefined,
  active: boolean,
  project: (pack: CustomizationPack) => T,
) {
  const scope = usePacks((s) => s.scope);
  const entry = usePacks((s) => s.packs.find((p) => p.id === id));
  const [result, setResult] = useState<{
    scope: typeof scope;
    entry: typeof entry;
    value?: T;
    error?: boolean;
  }>();
  const [attempt, setAttempt] = useState(0);
  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  useEffect(() => {
    let current = true;
    setResult(undefined);
    if (!active || !id || !entry) return;
    void usePacks
      .getState()
      .readPack(
        id,
        () =>
          current &&
          usePacks.getState().scope === scope &&
          usePacks.getState().packs.find((p) => p.id === id) === entry,
      )
      .then((pack) => {
        if (
          current &&
          usePacks.getState().scope === scope &&
          usePacks.getState().packs.find((p) => p.id === id) === entry
        )
          setResult(
            pack
              ? { scope, entry, value: project(pack) }
              : { scope, entry, error: true },
          );
      })
      .catch(() => {
        if (
          current &&
          usePacks.getState().scope === scope &&
          usePacks.getState().packs.find((p) => p.id === id) === entry
        )
          setResult({ scope, entry, error: true });
      });
    return () => {
      current = false;
    };
  }, [scope, entry, id, active, project, attempt]);
  const settled =
    active && result?.scope === scope && result?.entry === entry
      ? result
      : undefined;
  return {
    value: settled?.value,
    error: Boolean(settled?.error),
    loading: Boolean(active && id && entry && !settled),
    retry,
  };
}

const identity = (pack: CustomizationPack) => pack;
export function usePack(id: string | undefined, active = true) {
  return usePackRequest(id, active).pack;
}
export function usePackRequest(id: string | undefined, active = true) {
  const result = usePackProjection(id, active, identity);
  return {
    pack: result.value,
    error: result.error,
    loading: result.loading,
    retry: result.retry,
  };
}
export function useStickerAsset(id: string | undefined) {
  const packs = usePacks((s) => s.packs);
  const packId = id ? stickerPackId(id, packs) : undefined;
  const projection = useCallback(
    (pack: CustomizationPack) => (id ? stickerById(id, [pack]) : undefined),
    [id],
  );
  return usePackProjection(packId, Boolean(id), projection).value;
}

/** Retain one card's selected image string, not its original library. */
export function useThemePreview(
  id: string,
  wallpaperId: string | undefined,
  visible: boolean,
) {
  const projection = useCallback(
    (pack: CustomizationPack) => {
      if (pack.kind !== "theme") return undefined;
      return (
        (
          pack.wallpapers?.find((item) => item.id === wallpaperId) ??
          pack.wallpapers?.[0]
        )?.url ??
        pack.wallpaper ??
        pack.crest
      );
    },
    [wallpaperId],
  );
  return usePackProjection(id, visible, projection);
}
