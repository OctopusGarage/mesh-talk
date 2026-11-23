const MAX_DURATION_MS = 30_000;
const MAX_BYTES = 2 * 1024 * 1024;

const formats = [
  { mime: "audio/mp4", extension: "m4a" },
  { mime: "audio/webm;codecs=opus", extension: "webm" },
  { mime: "audio/ogg;codecs=opus", extension: "ogg" },
];

/** The caller owns cancellation, including while the OS permission prompt is open. */
export async function recordVoiceClip(
  signal: AbortSignal,
  onRecording: (stop: () => void) => void,
): Promise<File | null> {
  if (signal.aborted) return null;
  const format =
    typeof MediaRecorder !== "undefined" &&
    formats.find(({ mime }) => MediaRecorder.isTypeSupported(mime));
  if (!format || !navigator.mediaDevices?.getUserMedia)
    throw new DOMException(
      "Audio recording is unavailable",
      "NotSupportedError",
    );

  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const release = () => stream.getTracks().forEach((track) => track.stop());
  if (signal.aborted) {
    release();
    return null;
  }

  let recorder: MediaRecorder;
  try {
    recorder = new MediaRecorder(stream, {
      mimeType: format.mime,
      audioBitsPerSecond: 64_000,
    });
  } catch (error) {
    release();
    throw error;
  }
  return new Promise<File | null>((resolve, reject) => {
    const chunks: Blob[] = [];
    let size = 0;
    let closed = false;
    let stopping = false;
    let deadline: ReturnType<typeof setTimeout> | undefined;
    const cleanup = () => {
      if (closed) return false;
      closed = true;
      clearTimeout(deadline);
      signal.removeEventListener("abort", cancel);
      release();
      return true;
    };
    const stop = () => {
      if (closed || stopping) return;
      stopping = true;
      try {
        if (recorder.state !== "inactive") recorder.stop();
        else if (cleanup()) resolve(null);
      } catch (error) {
        if (cleanup()) reject(error);
      }
    };
    function cancel() {
      stop();
      if (cleanup()) resolve(null);
    }
    recorder.addEventListener("dataavailable", (event) => {
      if (closed || !event.data.size) return;
      size += event.data.size;
      if (size > MAX_BYTES) {
        if (cleanup())
          reject(
            new DOMException(
              "Recording exceeds the size limit",
              "QuotaExceededError",
            ),
          );
        return;
      }
      chunks.push(event.data);
    });
    recorder.addEventListener("stop", () => {
      if (!cleanup()) return;
      resolve(
        !signal.aborted && size > 0 && size <= MAX_BYTES
          ? new File(chunks, `voice-note.${format.extension}`, {
              type: format.mime,
            })
          : null,
      );
    });
    recorder.addEventListener("error", () => {
      if (cleanup())
        reject(new DOMException("Audio recording failed", "NotReadableError"));
    });
    signal.addEventListener("abort", cancel, { once: true });
    try {
      recorder.start(250);
      deadline = setTimeout(stop, MAX_DURATION_MS);
      onRecording(stop);
    } catch (error) {
      if (cleanup()) reject(error);
    }
  });
}
