/** A single reveal owns its URL. Decode before starting the visible lifetime. */
export async function createProtectedImageUrl(
  bytes: ArrayBuffer,
  mime: string,
  signal?: AbortSignal,
): Promise<string> {
  if (signal?.aborted) throw new Error("Reveal cancelled");
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  const image = new Image();
  let revoked = false;
  const revoke = () => {
    if (!revoked) {
      revoked = true;
      URL.revokeObjectURL(url);
    }
    image.src = "";
  };
  let rejectAbort!: (error: Error) => void;
  const aborted = new Promise<never>((_, reject) => {
    rejectAbort = reject;
  });
  const abort = () => {
    revoke();
    rejectAbort(new Error("Reveal cancelled"));
  };
  signal?.addEventListener("abort", abort, { once: true });
  try {
    image.src = url;
    await Promise.race([image.decode(), aborted]);
    image.src = "";
    return url;
  } catch (error) {
    revoke();
    throw error;
  } finally {
    signal?.removeEventListener("abort", abort);
  }
}
