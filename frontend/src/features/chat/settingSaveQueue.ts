/** Keeps dialog writes ordered, including blur saves queued while another write runs. */
export function createSettingSaveQueue(setBusy: (busy: boolean) => void) {
  let tail: Promise<void> = Promise.resolve();
  let pending = 0;
  return {
    whenIdle(): Promise<void> {
      return tail;
    },
    enqueue(
      write: () => Promise<void>,
      apply: () => void,
      onError: (error: unknown) => void,
    ): Promise<void> {
      pending++;
      setBusy(true);
      const task = tail.then(async () => {
        try {
          await write();
          apply();
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
