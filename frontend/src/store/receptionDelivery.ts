export interface ReceptionCursor {
  epoch: string;
  sequence: string;
}

export interface ReceptionScope {
  owner: string;
  account_id: string;
  device_id: string;
  runtime_id: string;
}

export interface ReceptionPage<Receipt> {
  scope: ReceptionScope;
  token: string;
  after: ReceptionCursor;
  next: ReceptionCursor;
  through: ReceptionCursor;
  complete: boolean;
  receipts: Receipt[];
}

export interface ReceptionDeliveryOptions<Receipt> {
  scope: ReceptionScope;
  cursor: ReceptionCursor | null;
  current: () => boolean;
  pull: (
    after: ReceptionCursor | null,
    through: ReceptionCursor | null,
  ) => Promise<ReceptionPage<Receipt>>;
  apply: (page: ReceptionPage<Receipt>) => void;
  persist: (cursor: ReceptionCursor) => void;
  acknowledge: (page: ReceptionPage<Receipt>) => Promise<boolean>;
  failed: (error: unknown) => void;
  yield?: () => Promise<void>;
  retryMs?: number;
}

/** One in-flight demand page and one dirty bit per exact runtime owner. */
export function createReceptionDelivery<Receipt>(
  options: ReceptionDeliveryOptions<Receipt>,
) {
  let stopped = false;
  let dirty = false;
  let active: Promise<void> | null = null;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let cursor = options.cursor;
  let through: ReceptionCursor | null = null;
  let pending: {
    page: ReceptionPage<Receipt>;
    applied: boolean;
    persisted: boolean;
  } | null = null;
  const current = () => !stopped && options.current();
  const yieldPage =
    options.yield ??
    (() => new Promise<void>((resolve) => setTimeout(resolve, 0)));
  function cancelRetry() {
    if (retry !== null) clearTimeout(retry);
    retry = null;
  }
  function sameScope(actual: ReceptionScope) {
    return (
      actual.owner === options.scope.owner &&
      actual.account_id === options.scope.account_id &&
      actual.device_id === options.scope.device_id &&
      actual.runtime_id === options.scope.runtime_id
    );
  }
  async function drain() {
    try {
      while (current() && (pending !== null || dirty)) {
        if (pending === null) {
          dirty = false;
          const page = await options.pull(cursor, through);
          if (!current()) return;
          if (!sameScope(page.scope))
            throw new Error("Reception page belongs to a replaced runtime");
          pending = { page, applied: false, persisted: false };
        }
        const work = pending;
        if (!current()) return;
        if (!work.applied) {
          options.apply(work.page);
          work.applied = true;
        }
        if (!current()) return;
        if (!work.persisted) {
          // The owning store writes its reception state and this cursor in one
          // storage operation. Failure retains this page without replaying it.
          options.persist(work.page.next);
          work.persisted = true;
        }
        if (!current()) return;
        const more = await options.acknowledge(work.page);
        if (!current()) return;
        cursor = work.page.next;
        through = work.page.complete ? null : work.page.through;
        pending = null;
        dirty ||= !work.page.complete || more;
        if (dirty) await yieldPage();
      }
    } catch (error) {
      if (!current()) return;
      options.failed(error);
      cancelRetry();
      retry = setTimeout(() => {
        retry = null;
        void wake();
      }, options.retryMs ?? 1000);
    }
  }
  function wake(): Promise<void> {
    if (!current()) return Promise.resolve();
    if (active !== null) {
      dirty = true;
      return active;
    }
    cancelRetry();
    // Retrying a page resumes at its failed step. It is not a second arrival.
    if (pending === null) dirty = true;
    active = drain().finally(() => {
      active = null;
    });
    return active;
  }
  return {
    wake,
    stop() {
      stopped = true;
      dirty = false;
      pending = null;
      cancelRetry();
    },
  };
}
