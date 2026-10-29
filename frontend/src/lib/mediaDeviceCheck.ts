/** Device-local diagnostics. No signaling, remote route or audible microphone echo. */
export type MediaDeviceKind = "audio" | "video";
export type MediaDeviceCheckCode =
  | "permission"
  | "notFound"
  | "busy"
  | "unsupported"
  | "timeout"
  | "cancelled"
  | "failed";
export class MediaDeviceCheckError extends Error {
  constructor(public readonly code: MediaDeviceCheckCode) {
    super(code);
    this.name = "MediaDeviceCheckError";
  }
}
export interface MediaLoopbackResult {
  bytesReceived: number;
  framesDecoded: number;
  totalSamplesReceived: number;
}
export interface MediaDeviceCheckSession {
  stream: MediaStream;
  devices: number;
  stop(): void;
  runLoopback(): Promise<MediaLoopbackResult>;
}
function failure(error: unknown): MediaDeviceCheckError {
  if (error instanceof MediaDeviceCheckError) return error;
  const name =
    error && typeof error === "object" && "name" in error ? error.name : "";
  return new MediaDeviceCheckError(
    name === "NotAllowedError" || name === "SecurityError"
      ? "permission"
      : name === "NotFoundError" || name === "OverconstrainedError"
        ? "notFound"
        : name === "NotReadableError"
          ? "busy"
          : "failed",
  );
}
/** Every asynchronous boundary is abortable and bounded, including native promises that never settle. */
function bounded<T>(
  task: Promise<T>,
  signal: AbortSignal,
  timeoutMs: number,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const finish = (result: () => void) => {
      clearTimeout(timer);
      signal.removeEventListener("abort", abort);
      result();
    };
    const abort = () =>
      finish(() => reject(new MediaDeviceCheckError("cancelled")));
    void task.catch(() => {});
    if (signal.aborted) {
      reject(new MediaDeviceCheckError("cancelled"));
      return;
    }
    signal.addEventListener("abort", abort, { once: true });
    const timer = setTimeout(
      () => finish(() => reject(new MediaDeviceCheckError("timeout"))),
      timeoutMs,
    );
    task.then(
      (value) => finish(() => resolve(value)),
      (error) => finish(() => reject(failure(error))),
    );
  });
}
export async function openMediaDeviceCheck(
  kind: MediaDeviceKind,
  signal: AbortSignal,
  options: { timeoutMs?: number } = {},
): Promise<MediaDeviceCheckSession> {
  if (signal.aborted) throw new MediaDeviceCheckError("cancelled");
  if (
    !navigator.mediaDevices?.getUserMedia ||
    !navigator.mediaDevices.enumerateDevices
  )
    throw new MediaDeviceCheckError("unsupported");
  const timeout = options.timeoutMs ?? 10_000;
  const lifetime = new AbortController();
  let stream: MediaStream | undefined;
  let stopped = false;
  let peers: RTCPeerConnection[] = [];
  let sink: HTMLVideoElement | undefined;
  let checking = false;
  const releasePeers = () => {
    if (sink) {
      sink.pause();
      sink.srcObject = null;
      sink.remove();
      sink = undefined;
    }
    for (const peer of peers) {
      peer.onicecandidate = null;
      peer.ontrack = null;
      peer.close();
    }
    peers = [];
  };
  const stop = () => {
    if (stopped) return;
    stopped = true;
    lifetime.abort();
    signal.removeEventListener("abort", stop);
    releasePeers();
    for (const track of stream?.getTracks() ?? []) track.stop();
  };
  signal.addEventListener("abort", stop, { once: true });
  try {
    const capture = navigator.mediaDevices.getUserMedia({
      audio: kind === "audio",
      video: kind === "video",
    });
    void capture.then(
      (value) => {
        if (stopped) for (const track of value.getTracks()) track.stop();
        else stream = value;
      },
      () => {},
    );
    stream = await bounded(capture, lifetime.signal, timeout);
    if (stopped) throw new MediaDeviceCheckError("cancelled");
    if (
      !stream
        .getTracks()
        .some((track) => track.kind === kind && track.readyState !== "ended")
    )
      throw new MediaDeviceCheckError("notFound");
    const devices = await bounded(
      navigator.mediaDevices.enumerateDevices(),
      lifetime.signal,
      timeout,
    );
    const ownedStream = stream;
    return {
      stream: ownedStream,
      devices: devices.filter(
        (device) =>
          device.kind === (kind === "audio" ? "audioinput" : "videoinput"),
      ).length,
      stop,
      async runLoopback() {
        if (stopped) throw new MediaDeviceCheckError("cancelled");
        if (checking) throw new MediaDeviceCheckError("busy");
        if (typeof RTCPeerConnection === "undefined") {
          stop();
          throw new MediaDeviceCheckError("unsupported");
        }
        checking = true;
        try {
          const sender = new RTCPeerConnection({ iceServers: [] });
          peers.push(sender);
          const receiver = new RTCPeerConnection({ iceServers: [] });
          peers.push(receiver);
          sender.onicecandidate = (event) => {
            if (!stopped && event.candidate)
              void receiver.addIceCandidate(event.candidate).catch(() => {});
          };
          receiver.onicecandidate = (event) => {
            if (!stopped && event.candidate)
              void sender.addIceCandidate(event.candidate).catch(() => {});
          };
          receiver.ontrack = (event) => {
            if (stopped) return;
            if (!sink) {
              sink = document.createElement("video");
              sink.muted = true;
              sink.autoplay = true;
              sink.playsInline = true;
              sink.style.cssText =
                "position:fixed;width:1px;height:1px;left:0;bottom:0;opacity:0.01;pointer-events:none";
              sink.setAttribute("aria-hidden", "true");
              document.body.append(sink);
            }
            sink.srcObject = event.streams[0] ?? new MediaStream([event.track]);
            void sink.play().catch(() => {});
          };
          const current = () => {
            if (stopped) throw new MediaDeviceCheckError("cancelled");
          };
          const negotiate = async () => {
            for (const track of ownedStream.getTracks())
              sender.addTrack(track, ownedStream);
            const offer = await sender.createOffer();
            current();
            await sender.setLocalDescription(offer);
            current();
            await receiver.setRemoteDescription(offer);
            current();
            const answer = await receiver.createAnswer();
            current();
            await receiver.setLocalDescription(answer);
            current();
            await sender.setRemoteDescription(answer);
            current();
          };
          await bounded(negotiate(), lifetime.signal, timeout);
          const until = Date.now() + timeout;
          let previous: MediaLoopbackResult | undefined;
          let sawMetric = false;
          while (Date.now() < until) {
            current();
            const stats = await bounded(
              receiver.getStats(),
              lifetime.signal,
              Math.max(1, until - Date.now()),
            );
            current();
            const sample: MediaLoopbackResult = {
              bytesReceived: 0,
              framesDecoded: 0,
              totalSamplesReceived: 0,
            };
            stats.forEach((report) => {
              if (
                report.type !== "inbound-rtp" ||
                (report.kind ?? report.mediaType) !== kind
              )
                return;
              const metric =
                kind === "audio"
                  ? report.totalSamplesReceived
                  : report.framesDecoded;
              if (typeof metric === "number") sawMetric = true;
              sample.bytesReceived += Number(report.bytesReceived) || 0;
              sample.framesDecoded += Number(report.framesDecoded) || 0;
              sample.totalSamplesReceived +=
                Number(report.totalSamplesReceived) || 0;
            });
            if (
              previous &&
              sample.bytesReceived > previous.bytesReceived &&
              (kind === "audio"
                ? sample.totalSamplesReceived > previous.totalSamplesReceived
                : sample.framesDecoded > previous.framesDecoded)
            )
              return sample;
            previous = sample;
            await bounded(
              new Promise<void>((resolve) => setTimeout(resolve, 100)),
              lifetime.signal,
              Math.max(1, until - Date.now()),
            );
          }
          throw new MediaDeviceCheckError(
            sawMetric ? "timeout" : "unsupported",
          );
        } catch (error) {
          stop();
          throw failure(error);
        } finally {
          checking = false;
          releasePeers();
        }
      },
    };
  } catch (error) {
    stop();
    throw failure(error);
  }
}
