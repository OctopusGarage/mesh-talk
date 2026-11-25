// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { VoiceCaptureDialog } from "./VoiceCaptureDialog";
const recording = vi.hoisted(() => vi.fn());
const dialog = vi.hoisted(() => ({ change: (_open: boolean) => {} }));
vi.mock("./attachments/voiceRecording", () => ({ recordVoiceClip: recording }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: ({
      children,
      onOpenChange,
    }: {
      children: ReactNode;
      onOpenChange: (open: boolean) => void;
    }) => {
      dialog.change = onOpenChange;
      return createElement(Wrap, null, children);
    },
    DialogContent: Wrap,
    DialogTitle: Wrap,
    DialogDescription: Wrap,
  };
});
const host = document.createElement("div");
let root = createRoot(host);
afterEach(async () => {
  await act(async () => root.unmount());
  root = createRoot(host);
  vi.restoreAllMocks();
  recording.mockReset();
});
function button(label: string) {
  return [...host.querySelectorAll("button")].find(
    (b) => b.textContent === label,
  )!;
}
it("stops recording, previews, retries a refused send, and revokes the preview", async () => {
  let finish!: (file: File) => void;
  const file = new File(["audio"], "voice.m4a");
  const stop = vi.fn(() => finish(file));
  recording.mockImplementation((_signal, ready) => {
    ready(stop);
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  const create = vi.fn(() => "blob:voice");
  const revoke = vi.fn();
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: create,
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: revoke,
  });
  const send = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
  await act(async () =>
    root.render(
      createElement(VoiceCaptureDialog, { onCancel: vi.fn(), onSend: send }),
    ),
  );
  await act(async () => button("voiceCapture.record").click());
  await act(async () => button("voiceCapture.stop").click());
  expect(stop).toHaveBeenCalledOnce();
  expect(host.querySelector("audio")?.getAttribute("src")).toBe("blob:voice");
  await act(async () => button("voiceCapture.send").click());
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
  await act(async () => button("voiceCapture.send").click());
  expect(send).toHaveBeenNthCalledWith(2, file);
  expect(button("common.cancel").disabled).toBe(true);
  await act(async () => root.unmount());
  root = createRoot(host);
  expect(revoke).toHaveBeenCalledWith("blob:voice");
});
it.each([null, new Error("permission denied")])(
  "reports capture failure %s and permits cancellation",
  async (outcome) => {
    recording.mockImplementation(async () => {
      if (outcome instanceof Error) throw outcome;
      return outcome;
    });
    const cancel = vi.fn();
    await act(async () =>
      root.render(
        createElement(VoiceCaptureDialog, {
          onCancel: cancel,
          onSend: vi.fn(),
        }),
      ),
    );
    await act(async () => button("voiceCapture.record").click());
    expect(host.querySelector('[role="alert"]')?.textContent).toBe(
      "voiceCapture.failed",
    );
    await act(async () => button("common.cancel").click());
    expect(cancel).toHaveBeenCalledOnce();
  },
);
it("aborts an in-flight microphone request when dismissed", async () => {
  let signal!: AbortSignal;
  let finish!: (file: null) => void;
  recording.mockImplementation((value) => {
    signal = value;
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  await act(async () =>
    root.render(
      createElement(VoiceCaptureDialog, { onCancel: vi.fn(), onSend: vi.fn() }),
    ),
  );
  await act(async () => button("voiceCapture.record").click());
  await act(async () => root.unmount());
  root = createRoot(host);
  expect(signal.aborted).toBe(true);
  await act(async () => finish(null));
});

it("honors dialog dismissal and ignores errors after capture is aborted", async () => {
  const cancel = vi.fn();
  let fail!: (error: Error) => void;
  recording.mockImplementation(
    () =>
      new Promise((_resolve, reject) => {
        fail = reject;
      }),
  );
  await act(async () =>
    root.render(
      createElement(VoiceCaptureDialog, { onCancel: cancel, onSend: vi.fn() }),
    ),
  );
  await act(async () => dialog.change(true));
  expect(cancel).not.toHaveBeenCalled();
  await act(async () => dialog.change(false));
  expect(cancel).toHaveBeenCalledOnce();
  await act(async () => button("voiceCapture.record").click());
  await act(async () => root.unmount());
  root = createRoot(host);
  await act(async () => fail(new Error("cancelled permission request")));
  expect(host.querySelector('[role="alert"]')).toBeNull();
});
