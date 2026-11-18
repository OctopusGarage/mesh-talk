import { create } from "zustand";

export interface OutgoingMediaPreview {
  url: string;
  current: () => boolean;
  release: () => void;
}

type RetainedPreview = OutgoingMediaPreview & {
  bytes: number;
  deadline: number;
  revoke: () => void;
};
type Previews = Map<string, RetainedPreview>;

const maxPreviewBytes = 3 * 1024 ** 2;
const maxRetainedBytes = 12 * 1024 ** 2;
const maxRetainedEntries = 8;
const retentionMs = 30_000;
const usePreviews = create<Previews>(() => new Map());
let timer:
  { deadline: number; handle: ReturnType<typeof setTimeout> } | undefined;
let resetGeneration = 0;

function makePreview(
  key: string,
  url: string,
  bytes: number,
  urlApi: typeof URL,
  ownerCurrent: () => boolean,
): RetainedPreview {
  const deadline = performance.now() + retentionMs;
  let retained = true;
  const entry: RetainedPreview = {
    url,
    bytes,
    deadline,
    current: () => retained && performance.now() < deadline && ownerCurrent(),
    release: () => {
      if (usePreviews.getState().get(key) === entry) releasePreviews(key);
    },
    revoke: () => {
      if (!retained) return;
      retained = false;
      urlApi.revokeObjectURL(url);
    },
  };
  return entry;
}

function scheduleExpiry(entries: Previews) {
  // All entries have the same lifetime, so insertion order is deadline order.
  const deadline = entries.values().next().value?.deadline;
  if (timer?.deadline === deadline) return;
  if (timer) clearTimeout(timer.handle);
  timer = undefined;
  if (deadline !== undefined) {
    const handle = setTimeout(
      () => {
        timer = undefined;
        releasePreviews();
      },
      Math.max(0, Math.ceil(deadline - performance.now())),
    );
    timer = { deadline, handle };
  }
}

function publish(entries: Previews) {
  scheduleExpiry(entries);
  usePreviews.setState(entries, true);
}

function prune(entries: Previews, key?: string) {
  let bytes = 0;
  for (const [entryKey, entry] of entries) {
    if (entryKey === key || !entry.current()) {
      entries.delete(entryKey);
      entry.revoke();
    } else {
      bytes += entry.bytes;
    }
  }
  return bytes;
}

function releasePreviews(key?: string) {
  const previous = usePreviews.getState();
  const entries = new Map(previous);
  prune(entries, key);
  if (entries.size !== previous.size) publish(entries);
  else scheduleExpiry(entries);
}

/** Retain the exact locally submitted image for the optimistic message. */
export function retainOutgoingMediaPreview(
  key: string,
  blob: Blob,
  current: () => boolean,
): void {
  const previous = usePreviews.getState();
  const entries = new Map(previous);
  const generation = resetGeneration;
  const urlApi = globalThis.URL;
  const bytes = blob.size;
  const canCreate =
    bytes > 0 &&
    bytes <= maxPreviewBytes &&
    blob.type.startsWith("image/") &&
    typeof urlApi?.createObjectURL === "function" &&
    typeof urlApi.revokeObjectURL === "function" &&
    current();
  let retainedBytes = prune(entries, canCreate ? key : undefined);

  if (canCreate) {
    for (const [oldKey, oldEntry] of entries) {
      if (
        entries.size < maxRetainedEntries &&
        retainedBytes + bytes <= maxRetainedBytes
      )
        break;
      retainedBytes -= oldEntry.bytes;
      entries.delete(oldKey);
      oldEntry.revoke();
    }

    let url: string | undefined;
    try {
      if (current()) url = urlApi.createObjectURL(blob);
    } catch {
      // Preview creation is optional and does not affect the send.
    }
    if (generation !== resetGeneration || usePreviews.getState() !== previous) {
      if (url) urlApi.revokeObjectURL(url);
      return;
    }
    if (url) {
      if (current()) {
        entries.set(key, makePreview(key, url, bytes, urlApi, current));
        publish(entries);
        return;
      }
      urlApi.revokeObjectURL(url);
    }
  }
  if (entries.size !== previous.size) publish(entries);
}

export function releaseOutgoingMediaPreview(key: string): void {
  releasePreviews(key);
}

export function resetOutgoingMediaPreviews(): void {
  resetGeneration++;
  const entries = usePreviews.getState();
  for (const entry of entries.values()) entry.revoke();
  if (entries.size > 0) publish(new Map());
}

function readPreview(entries: Previews, key?: string) {
  const entry = key === undefined ? undefined : entries.get(key);
  return entry?.current() ? entry : undefined;
}

export function getOutgoingMediaPreview(
  key?: string,
): OutgoingMediaPreview | undefined {
  return readPreview(usePreviews.getState(), key);
}

export function useOutgoingMediaPreview(
  key?: string,
): OutgoingMediaPreview | undefined {
  return usePreviews((entries) => readPreview(entries, key));
}

/** Reclaim previews whose optimistic or canonical row was retired. */
export function pruneOutgoingMediaPreviews(): void {
  releasePreviews();
}
