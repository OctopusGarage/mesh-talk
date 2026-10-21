/** UI read watchdog. Mutations must retain their acceptance/commit semantics.
 * Native work cannot be cancelled by a webview timeout: keep an explicit global
 * admission budget until the original operation settles, even after timeout. */
const MAX_PENDING_READS = 64;
let pending = 0;
export function readWithDeadline<T>(
  read: () => Promise<T>,
  timeoutMs = 10_000,
): Promise<T> {
  if (pending >= MAX_PENDING_READS)
    return Promise.reject(new Error("Native read capacity exhausted"));
  pending++;
  let operation: Promise<T>;
  try {
    operation = Promise.resolve(read());
  } catch (error) {
    pending--;
    return Promise.reject(error);
  }
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("Native read timed out")),
      timeoutMs,
    );
    operation.then(
      (value) => {
        pending--;
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        pending--;
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
