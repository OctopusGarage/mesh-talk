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
