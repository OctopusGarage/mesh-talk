/** Keeps dialog writes ordered, including blur saves queued while another write runs. */
export function createSettingSaveQueue(setBusy: (busy: boolean) => void) {
  let tail: Promise<void> = Promise.resolve();
  let pending = 0;
  return {
    whenIdle(): Promise<void> {
      return tail;
    },
    enqueue<T>(
      write: () => Promise<T>,
      apply: (value: T) => void,
      onError: (error: unknown) => void,
    ): Promise<void> {
      pending++;
      setBusy(true);
      const task = tail.then(async () => {
        try {
          const value = await write();
          apply(value);
        } catch (error) {
          onError(error);
        } finally {
          pending--;
          setBusy(pending > 0);
        }
      });
      tail = task.catch(() => {});
      return task;
    },
  };
}
