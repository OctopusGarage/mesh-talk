import { useEffect, useRef, useState } from "react";

type CopyStatus = "copied" | "failed" | null;

/** Clipboard feedback shared by copy controls, including cancelable sensitive copies. */
export function useCopyFeedback() {
  const [status, setStatus] = useState<CopyStatus>(null);
  const timeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  const generation = useRef(0);

  const reset = () => {
    generation.current++;
    if (timeout.current !== null) clearTimeout(timeout.current);
    timeout.current = null;
    setStatus(null);
  };

  useEffect(() => {
    const pending = timeout;
    const requests = generation;
    return () => {
      requests.current++;
      if (pending.current !== null) clearTimeout(pending.current);
    };
  }, []);

  const copy = async (write: () => Promise<boolean | void>) => {
    reset();
    const request = generation.current;
    try {
      if ((await write()) === false) return;
      if (request !== generation.current) return;
      setStatus("copied");
      timeout.current = setTimeout(() => {
        if (request !== generation.current) return;
        setStatus(null);
        timeout.current = null;
      }, 1200);
    } catch {
      if (request === generation.current) setStatus("failed");
    }
  };

  return { status, copy, reset };
}
