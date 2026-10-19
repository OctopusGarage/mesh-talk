/** Serializes local edits per key while keeping remote revisions separate. */
export function createLinkedPreferencePublisher(
  write: (key: string, value: string | null) => Promise<{ revision: number }>,
  onError: (error: unknown) => void,
) {
  let active = true;
  let enabled = false;
  const revisions = new Map<string, number>();
  const pending = new Set<string>();
  const dirty = new Map<string, string | null>();

  const send = (key: string, value: string | null) => {
    if (!active || !enabled || pending.has(key)) return;
    pending.add(key);
    void write(key, value)
      .then((record) => {
        if (!active) return;
        revisions.set(key, Math.max(revisions.get(key) ?? 0, record.revision));
        if (dirty.get(key) === value) dirty.delete(key);
      })
      .catch(onError)
      .finally(() => {
        pending.delete(key);
        const next = dirty.get(key);
        if (active && dirty.has(key) && next !== value) send(key, next!);
      });
  };
  const flush = () => {
    for (const [key, value] of dirty) send(key, value);
  };

  return {
    publish(key: string, value: string | null) {
      if (!active) return;
      dirty.set(key, value);
      send(key, value);
    },
    setEnabled(next: boolean) {
      enabled = next;
      if (next) flush();
    },
    flush,
    shouldApply(key: string, revision: number) {
      return (
        active &&
        !pending.has(key) &&
        !dirty.has(key) &&
        revision > (revisions.get(key) ?? 0)
      );
    },
    markApplied(key: string, revision: number) {
      revisions.set(key, Math.max(revisions.get(key) ?? 0, revision));
    },
    stop() {
      active = false;
      dirty.clear();
    },
  };
}
