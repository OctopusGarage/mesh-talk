/** Every roster caller shares one owner-scoped lane and one pending invalidation. */
export function createRosterRefresh(context: {
  capture(): { current(): boolean };
  refresh(hasPending: () => boolean): Promise<void>;
}) {
  type Run = { current(): boolean; dirty: boolean; promise?: Promise<void> };
  let run: Run | undefined;
  const start = (work: Run): Promise<void> => {
    const drain = async () => {
      do {
        work.dirty = false;
        await context.refresh(() => work.dirty);
      } while (work.current() && work.dirty);
    };
    const promise = drain().finally(() => {
      work.promise = undefined;
      if (work.current() && work.dirty) return start(work);
    });
    work.promise = promise;
    return promise;
  };
  return () => {
    if (!run?.current()) run = { ...context.capture(), dirty: false };
    if (!run.current()) return Promise.resolve();
    if (run.promise) {
      run.dirty = true;
      return run.promise;
    }
    return start(run);
  };
}
