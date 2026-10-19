import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "@/lib/error";

const idle = { busy: false, succeeded: false, error: null as string | null };

/** Keeps only the latest result for an action that may outlive its dialog. */
export function useAsyncFeedback() {
  const [state, setState] = useState(idle);
  const generation = useRef(0);
  const reset = useCallback(() => {
    generation.current++;
    setState(idle);
  }, []);
  const run = useCallback(async (operation: () => Promise<unknown>) => {
    const request = ++generation.current;
    setState({ busy: true, succeeded: false, error: null });
    try {
      await operation();
      if (request === generation.current)
        setState({ busy: false, succeeded: true, error: null });
    } catch (error) {
      if (request === generation.current)
        setState({ busy: false, succeeded: false, error: errorMessage(error) });
    }
  }, []);
  useEffect(
    () => () => {
      generation.current++;
    },
    [],
  );
  return { ...state, run, reset };
}
