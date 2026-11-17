/** Every roster caller shares one owner-scoped lane and one pending invalidation. */
export function createRosterRefresh<Input = void>(context: {
  capture(): { current(): boolean };
  refresh(hasPending: () => boolean, input: Input | undefined): Promise<void>;
}): (input?: Input) => Promise<void> {
  type Run = {
    current(): boolean;
    dirty: boolean;
    input?: Input;
    promise?: Promise<void>;
  };
  let run: Run | undefined;
  const start = (work: Run): Promise<void> => {
    const drain = async () => {
      do {
        work.dirty = false;
        const input = work.input;
        work.input = undefined;
        await context.refresh(() => work.dirty, input);
      } while (work.current() && work.dirty);
    };
    const promise = drain().finally(() => {
      work.promise = undefined;
      if (work.current() && work.dirty) return start(work);
    });
    work.promise = promise;
    return promise;
  };
  return (input?: Input) => {
    if (!run?.current()) run = { ...context.capture(), dirty: false };
    if (!run.current()) return Promise.resolve();
    run.input = input;
    if (run.promise) {
      run.dirty = true;
      return run.promise;
    }
    return start(run);
  };
}
