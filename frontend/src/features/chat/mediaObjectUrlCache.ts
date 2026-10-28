type Entry = {
  references: number;
  valid: boolean;
  url: string | null;
  bytes: number;
  ownerCurrent: () => boolean;
  result: Promise<string | null>;
  expiry?: ReturnType<typeof setTimeout>;
};

type Limits = { maxEntries: number; maxBytes: number; idleMs: number };

/** Ordinary inline media only. Active views hold references; idle URLs have an
 * LRU byte/count budget and short expiry. Protected reveals must not use this. */
export class MediaObjectUrlCache {
  private entries = new Map<string, Entry>();
  constructor(private readonly limits: Limits) {}

  peek(key: string): string | null {
    const entry = this.entries.get(key);
    if (!entry) return null;
    if (!entry.ownerCurrent()) {
      this.remove(key, entry);
      return null;
    }
    return entry.url;
  }

  acquire(
    key: string,
    load: (current: () => boolean) => Promise<Blob>,
    ownerCurrent: () => boolean,
  ) {
    this.invalidateStale();
    let entry = this.entries.get(key);
    if (!entry) {
      entry = {
        references: 0,
        valid: true,
        url: null,
        bytes: 0,
        ownerCurrent,
        result: Promise.resolve(null),
      };
      const created = entry;
      const current = () =>
        created.valid && created.references > 0 && created.ownerCurrent();
      created.result = Promise.resolve()
        .then(async () => {
          if (!current()) return null;
          const blob = await load(current);
          if (!current()) return null;
          created.bytes = blob.size;
          created.url = URL.createObjectURL(blob);
          return created.url;
        })
        .catch((error: unknown) => {
          this.remove(key, created);
          throw error;
        });
      this.entries.set(key, created);
    }
    if (entry.expiry !== undefined) clearTimeout(entry.expiry);
    entry.references++;
    const held = entry;
    let released = false;
    return {
      result: held.result,
      release: () => {
        if (released) return;
        released = true;
        held.references--;
        if (!held.valid || held.references > 0) return;
        if (
          !held.url ||
          !held.ownerCurrent() ||
          held.bytes > this.limits.maxBytes
        ) {
          this.remove(key, held);
          return;
        }
        // Releasing the last view makes this the most recently used idle item.
        this.entries.delete(key);
        this.entries.set(key, held);
        held.expiry = setTimeout(
          () => this.remove(key, held),
          this.limits.idleMs,
        );
        this.trimIdle();
      },
    };
  }

  invalidateStale() {
    for (const [key, entry] of this.entries) {
      if (!entry.ownerCurrent()) this.remove(key, entry);
    }
  }

  clear() {
    for (const [key, entry] of this.entries) this.remove(key, entry);
  }

  private remove(key: string, entry: Entry) {
    if (!entry.valid) return;
    entry.valid = false;
    if (this.entries.get(key) === entry) this.entries.delete(key);
    if (entry.expiry !== undefined) clearTimeout(entry.expiry);
    if (entry.url) URL.revokeObjectURL(entry.url);
    entry.url = null;
  }

  private trimIdle() {
    const idle = [...this.entries].filter(
      ([, entry]) => entry.references === 0,
    );
    let count = idle.length;
    let bytes = idle.reduce((sum, [, entry]) => sum + entry.bytes, 0);
    for (const [key, entry] of idle) {
      if (count <= this.limits.maxEntries && bytes <= this.limits.maxBytes)
        break;
      count--;
      bytes -= entry.bytes;
      this.remove(key, entry);
    }
  }
}
