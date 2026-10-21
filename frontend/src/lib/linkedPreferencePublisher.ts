/** Serializes local edits per key while keeping remote revisions separate. */
export function createLinkedPreferencePublisher(
  write: (
    key: string,
    value: string | null,
  ) => Promise<{ revision: number; device_id?: string }>,
  onError: (error: unknown) => void,
) {
  let active = true;
  let enabled = false;
  const revisions = new Map<string, { revision: number; author: string }>();
  const newer = (key: string, revision: number, author: string) => {
    const previous = revisions.get(key);
    return (
      !previous ||
      revision > previous.revision ||
      (revision === previous.revision && author > previous.author)
    );
  };
  const retain = (key: string, revision: number, author: string) => {
    if (newer(key, revision, author)) revisions.set(key, { revision, author });
  };
  const pending = new Set<string>();
  const failed = new Set<string>();
  const dirty = new Map<string, string | null>();

  const send = (key: string, value: string | null) => {
    if (
      !active ||
      !enabled ||
      pending.has(key) ||
      failed.has(key) ||
      pending.size >= 4
    )
      return;
    pending.add(key);
    void write(key, value)
      .then((record) => {
        if (!active) return;
        retain(key, record.revision, record.device_id ?? "");
        if (dirty.get(key) === value) dirty.delete(key);
      })
      .catch((error) => {
        failed.add(key);
        onError(error);
      })
      .finally(() => {
        pending.delete(key);
        const next = dirty.get(key);
        if (active && dirty.has(key) && next !== value) {
          failed.delete(key);
          send(key, next!);
        }
        if (active) flush();
      });
  };
  const flush = () => {
    for (const [key, value] of dirty) send(key, value);
  };

  return {
    publish(key: string, value: string | null) {
      if (!active) return;
      failed.delete(key);
      dirty.set(key, value);
      send(key, value);
    },
    setEnabled(next: boolean) {
      enabled = next;
      if (next) {
        failed.clear();
        flush();
      }
    },
    flush: () => {
      failed.clear();
      flush();
    },
    shouldApply(key: string, revision: number, author = "") {
      return (
        active &&
        !pending.has(key) &&
        !dirty.has(key) &&
        newer(key, revision, author)
      );
    },
    markApplied(key: string, revision: number, author = "") {
      retain(key, revision, author);
    },
    stop() {
      active = false;
      dirty.clear();
      failed.clear();
    },
  };
}
