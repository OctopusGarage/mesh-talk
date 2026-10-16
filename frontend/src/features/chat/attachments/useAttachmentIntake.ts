import { useEffect, useRef, useState } from "react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { TFunction } from "i18next";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import {
  IMAGE_EXTENSIONS,
  VIDEO_EXTENSIONS,
  isImage,
  isVideo,
} from "@/lib/mediaTypes";
import { captureChatOwnership, convKey, useChat } from "@/store/chat";

import { AttachmentBatchError } from "./attachmentBatch";

type Current = () => boolean;
const BYTE_LIMIT = 64 * 1024 * 1024;

function writable() {
  const { active, channels } = useChat.getState();
  return (
    !!active &&
    !(
      active.kind === "channel" &&
      channels.some(
        (channel) =>
          channel.channel_id === active.id &&
          (channel.archived ||
            channel.dissolved ||
            channel.left ||
            channel.pending_invite),
      )
    )
  );
}

/** Owns acquisition and staging only; the chat store owns the outgoing intent after handoff. */
export function useAttachmentIntake(t: TFunction) {
  const mounted = useRef(false);
  const conversationEpoch = useRef(0);
  const [dragOver, setDragOver] = useState(false);
  const [screenshotAvailable, setScreenshotAvailable] = useState(false);
  const [screenshot, setScreenshot] = useState<{
    bytes: Uint8Array;
    current: Current;
  } | null>(null);

  function capture(): Current {
    const lease = captureChatOwnership();
    const active = useChat.getState().active;
    const key = active ? convKey(active) : null;
    const epoch = conversationEpoch.current;
    return () =>
      mounted.current &&
      epoch === conversationEpoch.current &&
      lease.current() &&
      writable() &&
      key !== null &&
      useChat.getState().active !== null &&
      convKey(useChat.getState().active!) === key;
  }
  function report(error: unknown, current: Current) {
    if (current())
      useChat
        .getState()
        .setError(
          t("composer.couldntOpenFile", { error: errorMessage(error) }),
        );
  }
  async function stage(
    bytes: Uint8Array,
    ext: string,
    name: string | undefined,
    media: boolean,
    current: Current,
  ) {
    if (!current()) return false;
    if (bytes.byteLength > BYTE_LIMIT)
      throw new Error("Copied file exceeds 64 MB; use Attach instead");
    const path = await chat.writeTempFile(Array.from(bytes), ext, name);
    if (!current()) {
      await chat.deleteTempFile(path).catch(() => {});
      return false;
    }
    // The intent retains this path for its own retry lifecycle after handoff.
    await useChat.getState().sendFile(path, media);
    return true;
  }
  async function pick(kind: "file" | "directory" | "media") {
    const current = capture();
    if (!current()) return;
    try {
      const path = await openFileDialog({
        multiple: false,
        ...(kind === "directory" ? { directory: true } : {}),
        ...(kind === "media"
          ? {
              filters: [
                {
                  name: "Images & Video",
                  extensions: [...IMAGE_EXTENSIONS, ...VIDEO_EXTENSIONS],
                },
              ],
            }
          : {}),
      });
      if (current() && typeof path === "string")
        await useChat.getState().sendFile(path, kind === "media");
    } catch (error) {
      report(error, current);
    }
  }
  async function pasteFiles(files: File[]) {
    const current = capture();
    let handedOffCount = 0;
    for (const file of files) {
      if (!current()) return;
      try {
        if (file.size > BYTE_LIMIT)
          throw new Error("Copied file exceeds 64 MB; use Attach instead");
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (!current()) return;
        const ext =
          file.name.split(".").pop() || file.type.split("/")[1] || "bin";
        if (
          await stage(
            bytes,
            ext,
            file.name || undefined,
            file.type.startsWith("image/") ||
              isImage(file.name) ||
              isVideo(file.name),
            current,
          )
        ) {
          handedOffCount++;
        }
      } catch (error) {
        report(error, current);
        throw new AttachmentBatchError(error, handedOffCount);
      }
    }
  }
  async function captureScreenshot(hideWindow: boolean) {
    const current = capture();
    if (!current()) return;
    try {
      const bytes = await chat.captureScreen(hideWindow);
      if (current() && bytes.length > 0) setScreenshot({ bytes, current });
    } catch (error) {
      if (!current()) return;
      const message = errorMessage(error);
      useChat
        .getState()
        .setError(
          message.includes("screen-recording-permission")
            ? t("screenshot.permission")
            : t("screenshot.failed", { error: message }),
        );
    }
  }
  async function sendScreenshot(bytes: Uint8Array) {
    const origin = screenshot;
    if (!origin?.current()) return;
    try {
      await stage(bytes, "png", undefined, true, origin.current);
      setScreenshot((value) => (value === origin ? null : value));
    } catch (error) {
      report(error, origin.current);
      throw error;
    }
  }

  useEffect(() => {
    mounted.current = true;
    conversationEpoch.current++;
    const unsubscribe = useChat.subscribe((state, previous) => {
      const key = state.active ? convKey(state.active) : null;
      const previousKey = previous.active ? convKey(previous.active) : null;
      if (
        key !== previousKey ||
        state.runEpoch !== previous.runEpoch ||
        state.identityEpoch !== previous.identityEpoch
      ) {
        conversationEpoch.current++;
        setScreenshot(null);
        setDragOver(false);
      }
    });
    let available = true;
    void chat.screenshotAvailable().then(
      (value) => {
        if (available) setScreenshotAvailable(value);
      },
      () => {
        if (available) setScreenshotAvailable(false);
      },
    );
    return () => {
      mounted.current = false;
      available = false;
      unsubscribe();
    };
  }, []);
  useEffect(() => {
    let listening = true;
    const current = capture();
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (!listening) return;
      const payload = event.payload;
      if (payload.type === "over") setDragOver(writable());
      else if (payload.type === "leave") setDragOver(false);
      else if (payload.type === "drop") {
        setDragOver(false);
        const current = capture();
        void (async () => {
          for (const path of payload.paths) {
            if (!current()) return;
            try {
              await useChat
                .getState()
                .sendFile(path, isImage(path) || isVideo(path));
            } catch (error) {
              report(error, current);
            }
          }
        })();
      }
    });
    const registered = unlisten.then(
      (stop) => {
        if (!listening) {
          stop();
          return;
        }
        return stop;
      },
      (error) => {
        if (listening) report(error, current);
      },
    );
    return () => {
      listening = false;
      void registered.then((stop) => stop?.());
    };
    // The subscription reads runtime state at each event and captures its own lease.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t]);

  return {
    dragOver,
    screenshotAvailable,
    screenshotBytes: screenshot?.current() ? screenshot.bytes : null,
    attachFile: () => pick("file"),
    attachDirectory: () => pick("directory"),
    pickMedia: () => pick("media"),
    pasteFiles,
    captureScreenshot,
    sendScreenshot,
    cancelScreenshot: () => setScreenshot(null),
  };
}
