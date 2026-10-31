import { createSettingSaveQueue } from "./settingSaveQueue";
import { readWithDeadline } from "@/lib/readDeadline";

/** Owns ordered writes and guards state publication across account/view changes. */
export function createSettingsDialogSession(setBusy: (busy: boolean) => void) {
  const queue = createSettingSaveQueue(setBusy);
  let owner: string | null = null;
  let ownerGeneration = 0;
  let viewGeneration = 0;

  const isCurrent = (request: number) => request === viewGeneration;

  return {
    setOwner(next: string | null) {
      if (owner === next) return;
      owner = next;
      ownerGeneration++;
      viewGeneration++;
    },
    beginLoad() {
      return ++viewGeneration;
    },
    request: () => viewGeneration,
    invalidateView() {
      viewGeneration++;
    },
    isCurrent,
    whenIdle: queue.whenIdle,
    async load<T>(
      request: number,
      read: () => Promise<T>,
      apply: (value: T) => void,
      onError: (error: unknown) => void,
    ) {
      await queue.whenIdle();
      if (!isCurrent(request)) return;
      try {
        const value = await readWithDeadline(read);
        if (isCurrent(request)) apply(value);
      } catch (error) {
        if (isCurrent(request)) onError(error);
      }
    },
    save<T>(
      write: () => Promise<T>,
      apply: (value: T) => void,
      onError: (error: unknown) => void,
    ): Promise<void> {
      const request = viewGeneration;
      const account = ownerGeneration;
      return queue.enqueue(
        async () =>
          account === ownerGeneration
            ? ({ saved: true, value: await write() } as const)
            : ({ saved: false } as const),
        (result) => {
          if (result.saved && isCurrent(request)) apply(result.value);
        },
        (error) => {
          if (isCurrent(request)) onError(error);
        },
      );
    },
  };
}
