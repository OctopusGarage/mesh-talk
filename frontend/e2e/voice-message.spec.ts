import { expect, test } from "./tauri-mock";
import {
  enterChat,
  openBobDm,
  revealComposerAttachments,
} from "./helpers/session";

test("voice recording is released on cancel and sent only after review", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const NativeRecorder = window.MediaRecorder;
    window.MediaRecorder = class extends NativeRecorder {
      constructor(stream: MediaStream, options?: MediaRecorderOptions) {
        super(stream, options);
        document.documentElement.dataset.voiceBytes = "0";
        this.addEventListener("dataavailable", ({ data }) => {
          if (data.size) document.documentElement.dataset.voiceBytes = "1";
        });
      }
    };
    Object.defineProperty(navigator.mediaDevices, "getUserMedia", {
      value: async () => {
        const context = new AudioContext();
        const output = context.createMediaStreamDestination();
        const tone = context.createOscillator();
        tone.connect(output);
        tone.start();
        await context.resume();
        const track = output.stream.getAudioTracks()[0];
        const stop = track.stop.bind(track);
        track.stop = () => {
          stop();
          void context.close();
          document.documentElement.dataset.voiceTrack = track.readyState;
        };
        document.documentElement.dataset.voiceTrack = track.readyState;
        return output.stream;
      },
    });
  });
  await enterChat(page);
  await openBobDm(page);
  const openVoice = async () => {
    await revealComposerAttachments(page);
    await page.getByTestId("composer-voice").click();
  };
  await openVoice();
  const voice = page.getByTestId("voice-capture-dialog");
  await voice.getByRole("button", { name: "Record", exact: true }).click();
  await expect(
    voice.getByRole("button", { name: "Stop recording" }),
  ).toBeVisible();
  await voice.getByRole("button", { name: "Cancel" }).click();
  await expect(voice).toHaveCount(0);
  await expect(page.locator("html")).toHaveAttribute(
    "data-voice-track",
    "ended",
  );

  await openVoice();
  await voice.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-voice-bytes", "1");
  await voice.getByRole("button", { name: "Stop recording" }).click();
  const preview = voice.locator("audio");
  await expect(preview).toHaveAttribute("src", /^blob:/u);
  await expect
    .poll(() => preview.evaluate((audio: HTMLAudioElement) => audio.readyState))
    .toBeGreaterThan(0);
  await expect(page.locator("html")).toHaveAttribute(
    "data-voice-track",
    "ended",
  );
  await expect(page.getByText(/^voice-note\./u)).toHaveCount(0);
  await voice.getByRole("button", { name: "Send voice message" }).click();
  await expect(voice).toHaveCount(0);
  await expect(
    page.getByTestId("virtuoso-item-list").getByText(/^voice-note\./u),
  ).toBeVisible();
});
