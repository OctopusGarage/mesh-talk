import { afterEach, expect, it, vi } from "vitest";
import { recordVoiceClip } from "./voiceRecording";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it("releases a microphone granted after the recording is cancelled", async () => {
  let grant!: (stream: MediaStream) => void;
  const stop = vi.fn();
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: () =>
        new Promise<MediaStream>((resolve) => (grant = resolve)),
    },
  });
  vi.stubGlobal(
    "MediaRecorder",
    class {
      static isTypeSupported = () => true;
    },
  );
  const controller = new AbortController();
  const recording = recordVoiceClip(controller.signal, () => {});
  controller.abort();
  grant({ getTracks: () => [{ stop }] } as unknown as MediaStream);
  await expect(recording).resolves.toBeNull();
  expect(stop).toHaveBeenCalledOnce();
});

it.each(["explicit stop", "duration limit"])(
  "releases the microphone before returning a clip at %s",
  async (trigger) => {
    vi.useFakeTimers();
    const stopTrack = vi.fn();
    vi.stubGlobal("navigator", {
      mediaDevices: {
        getUserMedia: async () => ({ getTracks: () => [{ stop: stopTrack }] }),
      },
    });
    vi.stubGlobal(
      "MediaRecorder",
      class extends EventTarget {
        static isTypeSupported = () => true;
        state = "inactive";
        start() {
          this.state = "recording";
        }
        stop() {
          this.state = "inactive";
          this.dispatchEvent(
            Object.assign(new Event("dataavailable"), {
              data: new Blob(["audio"]),
            }),
          );
          this.dispatchEvent(new Event("stop"));
        }
      },
    );
    const recording = recordVoiceClip(new AbortController().signal, (stop) => {
      if (trigger === "explicit stop") stop();
    });
    await vi.advanceTimersByTimeAsync(30_000);
    const clip = await recording;
    expect(vi.getTimerCount()).toBe(0);
    expect(stopTrack).toHaveBeenCalledOnce();
    expect(clip?.name).toBe("voice-note.m4a");
    expect(clip?.size).toBe(5);
  },
);

it.each(["construct", "start", "stop", "error", "oversize", "empty", "abort"])(
  "releases recording resources on %s",
  async (failure) => {
    vi.useFakeTimers();
    const release = vi.fn();
    let dispatch!: (event: Event) => boolean;
    vi.stubGlobal("navigator", {
      mediaDevices: {
        getUserMedia: async () => ({ getTracks: () => [{ stop: release }] }),
      },
    });
    vi.stubGlobal(
      "MediaRecorder",
      class extends EventTarget {
        static isTypeSupported = () => true;
        state = "inactive";
        constructor() {
          super();
          if (failure === "construct") throw new Error("construct");
          dispatch = (event) => this.dispatchEvent(event);
        }
        start() {
          if (failure === "start") throw new Error("start");
          this.state = "recording";
        }
        stop() {
          if (failure === "stop") throw new Error("stop");
          this.state = "inactive";
          this.dispatchEvent(new Event("stop"));
        }
      },
    );
    const controller = new AbortController();
    let stop!: () => void;
    const result = recordVoiceClip(controller.signal, (value) => {
      stop = value;
    });
    const settled = result.then(
      (value) => ({ value }),
      (error) => ({ error }),
    );
    await Promise.resolve();
    if (failure === "stop" || failure === "empty") {
      stop();
      stop();
    }
    if (failure === "error") dispatch(new Event("error"));
    if (failure === "oversize")
      dispatch(
        Object.assign(new Event("dataavailable"), {
          data: new Blob([new Uint8Array(2 * 1024 * 1024 + 1)]),
        }),
      );
    if (failure === "abort") controller.abort();
    const outcome = await settled;
    if (failure === "empty" || failure === "abort")
      expect(outcome).toEqual({ value: null });
    else expect(outcome).toHaveProperty("error");
    expect(release).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  },
);
it("does not request a microphone after cancellation or without a supported format", async () => {
  const request = vi.fn();
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: request } });
  const controller = new AbortController();
  controller.abort();
  await expect(recordVoiceClip(controller.signal, vi.fn())).resolves.toBeNull();
  vi.stubGlobal(
    "MediaRecorder",
    class {
      static isTypeSupported = () => false;
    },
  );
  await expect(
    recordVoiceClip(new AbortController().signal, vi.fn()),
  ).rejects.toMatchObject({ name: "NotSupportedError" });
  expect(request).not.toHaveBeenCalled();
});

it("settles an inactive recorder and ignores trailing native events", async () => {
  vi.useFakeTimers();
  const release = vi.fn();
  let emit!: (event: Event) => boolean;
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async () => ({ getTracks: () => [{ stop: release }] }),
    },
  });
  vi.stubGlobal(
    "MediaRecorder",
    class extends EventTarget {
      static isTypeSupported = () => true;
      state = "inactive";
      constructor() {
        super();
        emit = (event) => this.dispatchEvent(event);
      }
      start() {}
    },
  );
  let stop!: () => void;
  const pending = recordVoiceClip(new AbortController().signal, (value) => {
    stop = value;
  });
  await Promise.resolve();
  emit(Object.assign(new Event("dataavailable"), { data: new Blob([]) }));
  stop();
  emit(Object.assign(new Event("dataavailable"), { data: new Blob(["late"]) }));
  emit(new Event("stop"));
  emit(new Event("error"));
  await expect(pending).resolves.toBeNull();
  expect(release).toHaveBeenCalledOnce();
  expect(vi.getTimerCount()).toBe(0);
});
it("cancels recording when the native stop event is delayed", async () => {
  vi.useFakeTimers();
  const release = vi.fn();
  const nativeStop = vi.fn();
  vi.stubGlobal("navigator", {
    mediaDevices: {
      getUserMedia: async () => ({ getTracks: () => [{ stop: release }] }),
    },
  });
  vi.stubGlobal(
    "MediaRecorder",
    class extends EventTarget {
      static isTypeSupported = () => true;
      state = "recording";
      start() {}
      stop = nativeStop;
    },
  );
  const controller = new AbortController();
  const pending = recordVoiceClip(controller.signal, vi.fn());
  await Promise.resolve();
  controller.abort();
  await expect(pending).resolves.toBeNull();
  expect(nativeStop).toHaveBeenCalledOnce();
  expect(release).toHaveBeenCalledOnce();
  expect(vi.getTimerCount()).toBe(0);
});
