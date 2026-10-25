// @vitest-environment jsdom
import { act, createElement, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import { useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import { useDrafts } from "@/store/drafts";
import type { Composer } from "../Composer";
import type { ScreenshotEditor } from "../ScreenshotEditor";
import i18n from "@/lib/i18n";
import { useAttachmentIntake } from "./useAttachmentIntake";
import { ConversationView } from "../ConversationView";

const originalAdmitImage = useChat.getState().admitImage;
const originalAdmitTextAndWait = useChat.getState().admitTextAndWait;
const originalRetry = useChat.getState().retry;

const native = vi.hoisted(() => ({
  invoke: vi.fn(),
  open: vi.fn(),
  register: vi.fn(),
  drop: null as
    | null
    | ((event: {
        payload: { type: "over" | "leave" | "drop"; paths: string[] };
      }) => void),
}));
let editor: ComponentProps<typeof ScreenshotEditor> | null;
let composer: ComponentProps<typeof Composer>;
let actualComposer: typeof Composer | null = null;
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@/lib/api/language", () => ({}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: native.open }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (callback: NonNullable<typeof native.drop>) => {
      native.drop = callback;
      return native.register(callback);
    },
  }),
}));
vi.mock("react-virtuoso", () => ({
  Virtuoso: ({
    data = [],
    itemContent,
  }: {
    data?: import("@/store/chat").ChatMessage[];
    itemContent: (
      index: number,
      message: import("@/store/chat").ChatMessage,
    ) => import("react").ReactNode;
  }) => data.map((message, index) => itemContent(index, message)),
}));
vi.mock("../MessageBubble", () => ({
  MessageBubble: ({
    m,
    onReply,
  }: {
    m: import("@/store/chat").ChatMessage;
    onReply: (message: import("@/store/chat").ChatMessage) => void;
  }) =>
    createElement(
      "button",
      { onClick: () => onReply(m), "data-testid": `reply-${m.id}` },
      "Reply",
    ),
}));
vi.mock("../Composer", () => ({
  Composer: (props: ComponentProps<typeof Composer>) => {
    composer = props;
    return actualComposer ? createElement(actualComposer, props) : null;
  },
}));
vi.mock("../ScreenshotEditor", () => ({
  ScreenshotEditor: (props: ComponentProps<typeof ScreenshotEditor>) => {
    editor = props;
    return null;
  },
}));
vi.mock("../ConversationHeaders", () => ({
  ConversationBio: () => null,
  ChannelHeader: () => null,
  DmHeader: () => null,
}));
vi.mock("../ConversationHistoryDialog", () => ({
  ConversationHistoryDialog: () => null,
}));
vi.mock("../AttachmentReceiveSettings", () => ({
  AttachmentReceiveSettings: () => null,
}));
vi.mock("../CallDialog", () => ({ CallButtons: () => null }));
vi.mock("../VerifyContactDialog", () => ({ VerifyContactDialog: () => null }));
vi.mock("../MemberProfileDialog", () => ({ MemberProfileDialog: () => null }));
vi.mock("../TransferBar", () => ({ TransferBar: () => null }));
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
let root: Root;
let container: HTMLDivElement;
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  editor = null;
  actualComposer = null;
  native.invoke.mockResolvedValue(false);
  native.register.mockResolvedValue(() => {});
  useAuth.setState({
    user: { id: "owner", username: "alice", display_name: "Alice" },
  });
  // This fixture mounts ConversationView directly, without ChatApp's draft loader.
  useDrafts.setState({
    owner: "owner",
    generation: useAuth.getState().generation,
    texts: {},
    loaded: true,
    error: null,
  });
  useChat.setState({
    active: { kind: "account", id: "alice", name: "Alice" },
    ready: true,
    channels: [],
    messages: {},
    reactions: {},
    loading: false,
    error: null,
    sendFile: vi.fn().mockResolvedValue(undefined),
  });
  container = document.createElement("div");
  root = createRoot(container);
  await act(async () => root.render(createElement(ConversationView)));
});
afterEach(async () => {
  await act(async () => root.unmount());
  useDrafts.getState().reset();
  container.remove();
  vi.unstubAllGlobals();
});
it("deletes a pasted temp file when the conversation changes during staging", async () => {
  const path = deferred<string>();
  native.invoke.mockImplementation((command: string) =>
    command === "write_temp_file" ? path.promise : Promise.resolve(),
  );
  const file = new File(["image"], "photo.png", { type: "image/png" });
  Object.defineProperty(file, "arrayBuffer", {
    value: async () => new Uint8Array([1]).buffer,
  });
  let sending!: Promise<void>;
  await act(async () => {
    sending = composer.onPasteFiles!([file]);
  });
  await act(async () =>
    useChat.setState({ active: { kind: "account", id: "bob", name: "Bob" } }),
  );
  await act(async () => {
    path.resolve("/tmp/photo.png");
    await sending;
  });
  expect(native.invoke).toHaveBeenCalledWith("delete_temp_file", {
    path: "/tmp/photo.png",
  });
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

function file(name = "photo.png", type = "image/png") {
  const value = new File(["bytes"], name, { type });
  Object.defineProperty(value, "arrayBuffer", {
    value: vi.fn(async () => new Uint8Array([1, 2]).buffer),
    configurable: true,
  });
  return value;
}
async function switchConversation() {
  await act(async () =>
    useChat.setState({ active: { kind: "account", id: "bob", name: "Bob" } }),
  );
}
it.each(["owner", "runtime", "identity", "unmount"])(
  "cleans a delayed staged file after %s invalidation",
  async (kind) => {
    const path = deferred<string>();
    native.invoke.mockImplementation((command: string) =>
      command === "write_temp_file" ? path.promise : Promise.resolve(),
    );
    let sending!: Promise<void>;
    await act(async () => {
      sending = composer.onPasteFiles!([file()]);
    });
    await act(async () => {
      if (kind === "owner")
        useAuth.setState({ generation: useAuth.getState().generation + 1 });
      if (kind === "runtime")
        useChat.setState({ runEpoch: useChat.getState().runEpoch + 1 });
      if (kind === "identity")
        useChat.setState({
          identityEpoch: useChat.getState().identityEpoch + 1,
        });
      if (kind === "unmount") {
        root.unmount();
        root = createRoot(document.createElement("div"));
      }
    });
    await act(async () => {
      path.resolve("/tmp/stale");
      await sending;
    });
    expect(native.invoke).toHaveBeenCalledWith("delete_temp_file", {
      path: "/tmp/stale",
    });
    expect(useChat.getState().sendFile).not.toHaveBeenCalled();
  },
);
it("checks the lease after browser reads before materializing IPC bytes", async () => {
  const bytes = deferred<ArrayBuffer>();
  const value = file();
  Object.defineProperty(value, "arrayBuffer", {
    value: () => bytes.promise,
    configurable: true,
  });
  let sending!: Promise<void>;
  await act(async () => {
    sending = composer.onPasteFiles!([value]);
  });
  await switchConversation();
  await act(async () => {
    bytes.resolve(new Uint8Array([1]).buffer);
    await sending;
  });
  expect(native.invoke).not.toHaveBeenCalledWith(
    "write_temp_file",
    expect.anything(),
  );
});
it.each(["onAttach", "onAttachDirectory", "onImage"] as const)(
  "refuses stale native %s picker results even after returning to the original chat",
  async (action) => {
    const path = deferred<string>();
    native.open.mockReturnValue(path.promise);
    let pending!: unknown;
    await act(async () => {
      pending = composer[action]!();
    });
    await switchConversation();
    await act(async () =>
      useChat.setState({
        active: { kind: "account", id: "alice", name: "Alice" },
      }),
    );
    await act(async () => {
      path.resolve("/native/photo.png");
      await pending;
    });
    expect(useChat.getState().sendFile).not.toHaveBeenCalled();
  },
);
it("keeps native paths and directory/media intent without byte staging", async () => {
  native.open.mockResolvedValue("/native/photo.png");
  await act(async () => {
    await composer.onAttach!();
    await composer.onImage!();
    await composer.onAttachDirectory!();
  });
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    1,
    "/native/photo.png",
    false,
    null,
  );
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    2,
    "/native/photo.png",
    true,
    null,
  );
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    3,
    "/native/photo.png",
    false,
    null,
  );
  expect(native.open).toHaveBeenCalledWith({
    multiple: false,
    directory: true,
  });
  expect(native.open).toHaveBeenCalledWith(
    expect.objectContaining({
      filters: [
        expect.objectContaining({
          extensions: expect.arrayContaining(["png", "mp4"]),
        }),
      ],
    }),
  );
  expect(native.invoke).not.toHaveBeenCalledWith(
    "write_temp_file",
    expect.anything(),
  );
});
it("preserves paste filenames and MIME/extension classification", async () => {
  native.invoke.mockResolvedValue("/tmp/staged");
  await act(async () => {
    await composer.onPasteFiles!([
      file("Holiday.MP4", "application/octet-stream"),
      file("image.bin", "image/png"),
      file("notes.txt", "text/plain"),
    ]);
  });
  expect(native.invoke).toHaveBeenCalledWith("write_temp_file", {
    bytes: [1, 2],
    ext: "MP4",
    name: "Holiday.MP4",
  });
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    1,
    "/tmp/staged",
    true,
    null,
  );
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    2,
    "/tmp/staged",
    true,
    null,
  );
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    3,
    "/tmp/staged",
    false,
    null,
  );
});
it("rejects files above the IPC limit before reading and reports the original message", async () => {
  const value = file();
  Object.defineProperty(value, "size", { value: 64 * 1024 * 1024 + 1 });
  await act(async () => {
    await expect(composer.onPasteFiles!([value])).rejects.toThrow(
      "Copied file exceeds 64 MB; use Attach instead",
    );
  });
  expect(value.arrayBuffer).not.toHaveBeenCalled();
  expect(useChat.getState().error).toContain(
    "Copied file exceeds 64 MB; use Attach instead",
  );
});
it("retries only the remaining batch after a later staging failure and allows a new completed batch", async () => {
  const first = file("first.png");
  const second = file("second.png");
  const files = [first, second];
  native.invoke
    .mockResolvedValueOnce("/tmp/first")
    .mockRejectedValueOnce(new Error("disk full"));
  await act(async () => {
    await expect(composer.onPasteFiles!(files)).rejects.toThrow("disk full");
  });
  native.invoke.mockResolvedValue("/tmp/second");
  await act(async () => {
    await composer.onPasteFiles!(files.slice(1));
  });
  expect(useChat.getState().sendFile).toHaveBeenCalledTimes(2);
  await act(async () => {
    await composer.onPasteFiles!([first]);
  });
  expect(useChat.getState().sendFile).toHaveBeenCalledTimes(3);
});
it("does not publish a delayed capture into a different conversation", async () => {
  const capture = deferred<number[]>();
  native.invoke.mockReturnValue(capture.promise);
  let pending!: unknown;
  await act(async () => {
    pending = composer.onScreenshot!(true);
  });
  await switchConversation();
  await act(async () => {
    capture.resolve([1]);
    await pending;
  });
  expect(editor).toBeNull();
});
it("retains the editor's originating lease when its send callback outlives a conversation switch", async () => {
  native.invoke.mockResolvedValue([1]);
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  expect(editor).not.toBeNull();
  const send = editor!.onSend;
  await switchConversation();
  await act(async () => {
    await send(new Uint8Array([1]));
  });
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
  expect(native.invoke).not.toHaveBeenCalledWith(
    "write_temp_file",
    expect.anything(),
  );
});
it("discards screenshot confirmation after the owning account changes", async () => {
  native.invoke.mockResolvedValue([1]);
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  const confirm = editor!.onSend;
  await act(async () =>
    useAuth.setState({ generation: useAuth.getState().generation + 1 }),
  );
  await act(async () => {
    await confirm(new Uint8Array([1]));
  });
  expect(composer.stagedScreenshot).toBeNull();
  expect(native.invoke).not.toHaveBeenCalledWith(
    "write_temp_file",
    expect.anything(),
  );
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});
it("preserves capture cancellation and permission-specific errors", async () => {
  native.invoke.mockResolvedValue([]);
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  expect(editor).toBeNull();
  expect(useChat.getState().error).toBeNull();
  native.invoke.mockRejectedValue(new Error("screen-recording-permission"));
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  expect(useChat.getState().error).toBe(i18n.t("screenshot.permission"));
  native.invoke.mockRejectedValue(new Error("capture failed"));
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  expect(useChat.getState().error).toBe(
    i18n.t("screenshot.failed", { error: "capture failed" }),
  );
});
it("classifies native drops and stops a pending batch at a conversation switch", async () => {
  const sending = deferred<void>();
  vi.mocked(useChat.getState().sendFile).mockReturnValueOnce(sending.promise);
  await act(async () => {
    native.drop!({
      payload: {
        type: "drop",
        paths: ["/native/clip.mp4", "/native/notes.txt"],
      },
    });
  });
  await switchConversation();
  await act(async () => sending.resolve());
  expect(useChat.getState().sendFile).toHaveBeenCalledExactlyOnceWith(
    "/native/clip.mp4",
    true,
    null,
  );
});
it("does not enqueue the accepted prefix again when a later pasted file cannot be staged", async () => {
  useChat.setState({
    sendFile: useChat.getInitialState().sendFile,
    intents: {},
    reload: vi.fn().mockResolvedValue(undefined),
  });
  let stages = 0;
  native.invoke.mockImplementation(
    (command: string, args: { path?: string }) => {
      if (command === "write_temp_file") {
        stages++;
        if (stages === 2) return Promise.reject(new Error("disk full"));
        return Promise.resolve(stages === 1 ? "/tmp/first" : "/tmp/second");
      }
      if (command === "owner_enqueue_file")
        return Promise.resolve({
          id: args.path,
          fileConv: "file-conversation",
        });
      return Promise.resolve();
    },
  );
  const files = [file("first.png"), file("second.png")];
  await act(async () => {
    await expect(composer.onPasteFiles!(files)).rejects.toThrow("disk full");
  });
  await act(async () => {
    await composer.onPasteFiles!(files.slice(1));
  });
  const enqueued = native.invoke.mock.calls.filter(
    ([command]) => command === "owner_enqueue_file",
  );
  expect(enqueued.map(([, args]) => args.path)).toEqual([
    "/tmp/first",
    "/tmp/second",
  ]);
  expect(
    useChat.getState().messages["account:alice"].map((message) => message.id),
  ).toEqual(["/tmp/first", "/tmp/second"]);
});
it("keeps a store-owned failed intent for its existing retry instead of reporting a staging failure", async () => {
  useChat.setState({
    sendFile: useChat.getInitialState().sendFile,
    intents: {},
    reload: vi.fn().mockResolvedValue(undefined),
  });
  native.invoke.mockImplementation((command: string) =>
    command === "write_temp_file"
      ? Promise.resolve("/tmp/photo.png")
      : Promise.reject(new Error("enqueue unavailable")),
  );
  await act(async () => {
    await composer.onPasteFiles!([file()]);
  });
  const intent = Object.values(useChat.getState().intents)[0];
  expect(intent.message.failed).toBe(true);
  expect(intent.payload).toEqual({
    kind: "file",
    path: "/tmp/photo.png",
    media: true,
    expiresAtMs: null,
  });
  expect(native.invoke).not.toHaveBeenCalledWith(
    "delete_temp_file",
    expect.anything(),
  );
});
it("suppresses picker and capture errors after owner invalidation", async () => {
  let rejectPicker!: (error: Error) => void;
  let rejectCapture!: (error: Error) => void;
  native.open.mockReturnValue(
    new Promise((_, reject) => {
      rejectPicker = reject;
    }),
  );
  native.invoke.mockReturnValue(
    new Promise((_, reject) => {
      rejectCapture = reject;
    }),
  );
  let picker: unknown;
  let capture: unknown;
  await act(async () => {
    picker = composer.onAttach!();
    capture = composer.onScreenshot!(false);
  });
  await act(async () =>
    useAuth.setState({ generation: useAuth.getState().generation + 1 }),
  );
  await act(async () => {
    rejectPicker(new Error("old picker"));
    rejectCapture(new Error("old capture"));
    await Promise.all([picker, capture]);
  });
  expect(useChat.getState().error).toBeNull();
});
it("rejects native drop acquisition for a read-only channel", async () => {
  await act(async () =>
    useChat.setState({
      active: { kind: "channel", id: "team", name: "Team" },
      channels: [
        {
          channel_id: "team",
          name: "Team",
          owner: "owner",
          member_count: 1,
          archived: true,
        },
      ],
    }),
  );
  await act(async () =>
    native.drop!({ payload: { type: "drop", paths: ["/native/file"] } }),
  );
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it("allows a new intentional batch to reuse a File from an abandoned partial batch", async () => {
  const first = file("first.png");
  const second = file("second.png");
  native.invoke
    .mockResolvedValueOnce("/tmp/first")
    .mockRejectedValueOnce(new Error("disk full"));
  await act(async () => {
    await expect(composer.onPasteFiles!([first, second])).rejects.toThrow(
      "disk full",
    );
  });
  native.invoke.mockResolvedValue("/tmp/intentional-resend");
  await act(async () => {
    await composer.onPasteFiles!([first]);
  });
  expect(useChat.getState().sendFile).toHaveBeenNthCalledWith(
    2,
    "/tmp/intentional-resend",
    true,
    null,
  );
});
function IntakeProbe({ translate }: { translate: typeof i18n.t }) {
  useAttachmentIntake(translate);
  return null;
}
it("ignores retired native callbacks while their registration is still pending", async () => {
  const registered = deferred<() => void>();
  native.register.mockReturnValueOnce(registered.promise);
  await act(async () =>
    root.render(createElement(IntakeProbe, { translate: i18n.t })),
  );
  const oldDrop = native.drop!;
  await act(async () =>
    root.render(createElement(IntakeProbe, { translate: i18n.t.bind(i18n) })),
  );
  const currentDrop = native.drop!;
  await act(async () => {
    oldDrop({ payload: { type: "drop", paths: ["/native/file"] } });
    currentDrop({ payload: { type: "drop", paths: ["/native/file"] } });
  });
  const stop = vi.fn();
  await act(async () => registered.resolve(stop));
  expect(stop).toHaveBeenCalledOnce();
  expect(useChat.getState().sendFile).toHaveBeenCalledExactlyOnceWith(
    "/native/file",
    false,
  );
});

it("removes the handed-off prefix in the real Composer and preserves files pasted during staging", async () => {
  const { Composer: ActualComposer } =
    await vi.importActual<typeof import("../Composer")>("../Composer");
  function ComposedIntake() {
    const intake = useAttachmentIntake(i18n.t);
    return createElement(ActualComposer, {
      onSend: () => true,
      onPasteFiles: intake.pasteFiles,
      placeholder: "Message",
      mentionNames: [],
    });
  }
  await act(async () => root.render(createElement(ComposedIntake)));
  const paste = (files: File[]) => {
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", {
      value: { files, items: [] },
    });
    container.querySelector("textarea")!.dispatchEvent(event);
  };
  const first = file("first.png");
  const second = file("second.png");
  const later = file("later.png");
  const failedStage = deferred<string>();
  native.invoke
    .mockResolvedValueOnce("/tmp/first")
    .mockReturnValueOnce(failedStage.promise);
  await act(async () => paste([first, second]));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  await act(async () => paste([later]));
  // Delayed failure must retire only the prefix already handed to the store.
  await act(async () => failedStage.reject(new Error("disk full")));
  const pending = container.querySelector(
    '[data-testid="composer-pending-files"]',
  )!;
  expect(pending.textContent).not.toContain("first.png");
  expect(pending.textContent).toContain("second.png");
  expect(pending.textContent).toContain("later.png");
  native.invoke.mockResolvedValue("/tmp/rest");
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  expect(useChat.getState().sendFile).toHaveBeenCalledTimes(3);
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).toBeNull();
  await act(async () => paste([first]));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  expect(useChat.getState().sendFile).toHaveBeenCalledTimes(4);
});
it.each([false, true])(
  "consumes native registration rejection (retired: %s)",
  async (retired) => {
    const registration = deferred<() => void>();
    native.register.mockReturnValueOnce(registration.promise);
    await act(async () =>
      root.render(createElement(IntakeProbe, { translate: i18n.t })),
    );
    if (retired)
      await act(async () =>
        root.render(
          createElement(IntakeProbe, { translate: i18n.t.bind(i18n) }),
        ),
      );
    await act(async () =>
      registration.reject(new Error("drag listener unavailable")),
    );
    if (retired) expect(useChat.getState().error).toBeNull();
    else
      expect(useChat.getState().error).toContain("drag listener unavailable");
  },
);
it("preserves files pasted into the real Composer while an earlier batch succeeds", async () => {
  const { Composer: ActualComposer } =
    await vi.importActual<typeof import("../Composer")>("../Composer");
  function ComposedIntake() {
    const intake = useAttachmentIntake(i18n.t);
    return createElement(ActualComposer, {
      onSend: () => true,
      onPasteFiles: intake.pasteFiles,
      placeholder: "Message",
      mentionNames: [],
    });
  }
  await act(async () => root.render(createElement(ComposedIntake)));
  const paste = (files: File[]) => {
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", {
      value: { files, items: [] },
    });
    container.querySelector("textarea")!.dispatchEvent(event);
  };
  const staging = deferred<string>();
  native.invoke.mockReturnValueOnce(staging.promise);
  await act(async () => paste([file("first.png")]));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click(),
  );
  await act(async () => paste([file("later.png")]));
  await act(async () => staging.resolve("/tmp/first"));
  const pending = container.querySelector(
    '[data-testid="composer-pending-files"]',
  )!;
  expect(pending.textContent).toBe("later.png");
  expect(useChat.getState().sendFile).toHaveBeenCalledExactlyOnceWith(
    "/tmp/first",
    true,
  );
});

it("defaults new messages to Keep", () => {
  expect(composer.expiryDurationMs).toBeNull();
});

it("preserves the selected expiry through picker and paste intake after merging expiry controls", async () => {
  const now = vi.spyOn(Date, "now").mockReturnValue(1_000_000);
  try {
    await act(async () => composer.onExpiryDurationChange!(3_600_000));
    native.open.mockResolvedValue("/tmp/picked.txt");
    await act(async () => composer.onAttach!());
    expect(useChat.getState().sendFile).toHaveBeenLastCalledWith(
      "/tmp/picked.txt",
      false,
      4_600_000,
    );
    expect(composer.expiryDurationMs).toBeNull();
    await act(async () => composer.onExpiryDurationChange!(null));
    native.invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "write_temp_file" ? "/tmp/pasted.png" : false,
      ),
    );
    await act(async () => composer.onPasteFiles!([file()]));
    expect(useChat.getState().sendFile).toHaveBeenLastCalledWith(
      "/tmp/pasted.png",
      true,
      null,
    );
    expect(composer.expiryDurationMs).toBeNull();
  } finally {
    now.mockRestore();
  }
});

it("confirms a screenshot into the composer without writing or sending until explicit send", async () => {
  native.invoke.mockResolvedValue([1]);
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  native.invoke.mockClear();
  await act(async () => {
    await editor!.onSend(new Uint8Array([137, 80, 78, 71]));
  });
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
  expect(native.invoke).not.toHaveBeenCalledWith(
    "write_temp_file",
    expect.anything(),
  );
  expect(composer.stagedScreenshot).toBeInstanceOf(File);
  expect(composer.stagedScreenshot?.type).toBe("image/png");
});

it("clears an already staged screenshot when its owning account changes", async () => {
  native.invoke.mockResolvedValue([1]);
  await act(async () => {
    await composer.onScreenshot!(false);
  });
  await act(async () => {
    await editor!.onSend(new Uint8Array([1]));
  });
  expect(composer.stagedScreenshot).toBeInstanceOf(File);
  await act(async () =>
    useAuth.setState({ generation: useAuth.getState().generation + 1 }),
  );
  // Resume the composer after ChatApp would finish loading the new session's drafts.
  await act(async () =>
    useDrafts.setState({ generation: useAuth.getState().generation }),
  );
  expect(composer.stagedScreenshot).toBeNull();
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it("admits only one native screenshot acquisition at a time", async () => {
  const result = deferred<number[]>();
  native.invoke.mockImplementation((command: string) =>
    command === "capture_screen" ? result.promise : Promise.resolve(true),
  );
  let first!: unknown;
  await act(async () => {
    first = composer.onScreenshot!(false);
  });
  let second!: unknown;
  await act(async () => {
    second = composer.onScreenshot!(true);
  });
  const acquisitions = native.invoke.mock.calls.filter(
    ([command]) => command === "capture_screen",
  );
  await act(async () => {
    result.resolve([1]);
    await Promise.all([first, second]);
  });
  expect(acquisitions).toHaveLength(1);
  expect(editor).not.toBeNull();
});

it("does not reopen a cancelled screenshot when an acquisition completes late", async () => {
  let intake!: ReturnType<typeof useAttachmentIntake>;
  function Probe() {
    intake = useAttachmentIntake(i18n.t);
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  const result = deferred<number[]>();
  native.invoke.mockImplementation((command: string) =>
    command === "capture_screen" ? result.promise : Promise.resolve(true),
  );
  let first!: Promise<void>;
  await act(async () => {
    first = intake.captureScreenshot(false);
  });
  await act(async () => intake.cancelScreenshot());
  await act(async () => {
    result.resolve([1]);
    await first;
  });
  expect(intake.screenshotBytes).toBeNull();
});

it("snapshots image-picker protection and preserves a later mode selection", async () => {
  const picked = deferred<string>();
  native.open.mockReturnValueOnce(picked.promise);
  const admitImage = vi.fn().mockResolvedValue("accepted");
  useChat.setState({ admitImage });
  await act(async () => composer.onProtectionChange!("click"));
  let selecting!: Promise<void>;
  await act(async () => {
    selecting = composer.onImage!() as unknown as Promise<void>;
  });
  await act(async () => composer.onProtectionChange!("authenticate"));
  await act(async () => {
    picked.resolve("/tmp/private.png");
    await selecting;
  });
  expect(admitImage).toHaveBeenCalledWith(
    "/tmp/private.png",
    "click",
    null,
    expect.any(Function),
  );
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "other", name: "Other" },
    }),
  );
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "alice", name: "Alice" },
    }),
  );
  expect(composer.initialProtection).toBe("authenticate");
});

it("rejects a protected mixed attachment batch before any handoff", async () => {
  await act(async () => composer.onProtectionChange!("click"));
  const admitImage = vi.fn().mockResolvedValue("accepted");
  useChat.setState({ admitImage });
  await expect(
    composer.onPasteFiles!([
      new File(["image"], "photo.png", { type: "image/png" }),
      new File(["video"], "clip.mp4", { type: "video/mp4" }),
    ]),
  ).rejects.toThrow("support text and images only");
  expect(admitImage).not.toHaveBeenCalled();
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it("keeps failed image protection and rejects oversized protected image bytes before writing", async () => {
  await act(async () => composer.onProtectionChange!("authenticate"));
  const admitImage = vi.fn().mockResolvedValue("refused");
  useChat.setState({ admitImage });
  native.invoke.mockImplementation((command: string) =>
    command === "write_temp_file"
      ? Promise.resolve("/tmp/photo.png")
      : Promise.resolve(false),
  );
  const small = new File(["image"], "photo.png", { type: "image/png" });
  Object.defineProperty(small, "arrayBuffer", {
    value: async () => new Uint8Array([1]).buffer,
  });
  await act(async () => {
    await expect(composer.onPasteFiles!([small])).rejects.toThrow(
      "Image not accepted",
    );
  });
  expect(admitImage).toHaveBeenCalledWith(
    "/tmp/photo.png",
    "authenticate",
    null,
    expect.any(Function),
  );
  const huge = new File([], "large.png", { type: "image/png" });
  Object.defineProperty(huge, "size", { value: 16 * 1024 * 1024 + 1 });
  await act(async () => {
    await expect(composer.onPasteFiles!([huge])).rejects.toThrow("16 MiB");
  });
  expect(
    native.invoke.mock.calls.filter(
      ([command]) => command === "write_temp_file",
    ),
  ).toHaveLength(1);
});

it("applies protected image policy to dropped images and rejects dropped videos explicitly", async () => {
  await act(async () => composer.onProtectionChange!("click"));
  const admitImage = vi.fn().mockResolvedValue("accepted");
  useChat.setState({ admitImage });
  await act(async () =>
    native.drop!({ payload: { type: "drop", paths: ["/tmp/photo.png"] } }),
  );
  expect(admitImage).toHaveBeenCalledWith(
    "/tmp/photo.png",
    "click",
    null,
    expect.any(Function),
  );
  await act(async () => composer.onProtectionChange!("authenticate"));
  await act(async () =>
    native.drop!({ payload: { type: "drop", paths: ["/tmp/clip.mp4"] } }),
  );
  expect(admitImage).toHaveBeenCalledTimes(1);
  expect(useChat.getState().error).toContain("support text and images only");
});

it("retains verification for a partially accepted pasted batch and its retry", async () => {
  await act(async () => composer.onProtectionChange!("authenticate"));
  const admitImage = vi
    .fn()
    .mockResolvedValueOnce("accepted")
    .mockResolvedValueOnce("refused")
    .mockResolvedValueOnce("accepted");
  useChat.setState({ admitImage });
  native.invoke.mockImplementation(
    (command: string, args: { name?: string }) =>
      command === "write_temp_file"
        ? Promise.resolve(`/tmp/${args.name}`)
        : Promise.resolve(false),
  );
  const images = ["one.png", "two.png"].map((name) => {
    const image = new File(["bytes"], name, { type: "image/png" });
    Object.defineProperty(image, "arrayBuffer", {
      value: async () => new Uint8Array([1]).buffer,
    });
    return image;
  });
  await act(async () => {
    await expect(composer.onPasteFiles!(images)).rejects.toThrow(
      "Image not accepted",
    );
  });
  await act(async () => {
    composer.onProtectionChange!(undefined);
    await composer.onPasteFiles!([images[1]]);
  });
  expect(admitImage.mock.calls.map(([, mode]) => mode)).toEqual([
    "authenticate",
    "authenticate",
    "authenticate",
  ]);
});

it("does not consume protected text or staged image policy when an unrelated picker succeeds", async () => {
  await act(async () => {
    composer.onProtectionChange!("authenticate");
    composer.onDraftChange!("Keep this protected text");
    composer.onAttachmentDraftChange!(1);
  });
  const admitImage = vi.fn().mockResolvedValue("accepted");
  useChat.setState({ admitImage });
  native.open.mockResolvedValueOnce("/tmp/picked.png");
  await act(async () => {
    await composer.onImage!();
  });
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "other", name: "Other" },
    }),
  );
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "alice", name: "Alice" },
    }),
  );
  expect(composer.initialProtection).toBe("authenticate");
  expect(composer.initialDraft).toBe("Keep this protected text");
});

it.each(["none", "mode", "attachment", "replacement"])(
  "transfers retry ownership and consumes only unchanged protection (later edit: %s)",
  async (laterEdit) => {
    const { Composer: ActualComposer } =
      await vi.importActual<typeof import("../Composer")>("../Composer");
    useChat.setState({
      admitImage: originalAdmitImage,
      retry: originalRetry,
      intents: {},
      reload: vi.fn().mockResolvedValue(true),
    });
    await act(async () => composer.onProtectionChange!("authenticate"));
    actualComposer = ActualComposer;
    await act(async () => root.render(createElement(ConversationView)));
    let attempts = 0;
    native.invoke.mockImplementation((command: string) => {
      if (command === "write_temp_file")
        return Promise.resolve("/tmp/private.png");
      if (command === "owner_enqueue_file")
        return ++attempts === 1
          ? Promise.reject(new Error("offline"))
          : Promise.resolve({ id: "accepted-image", fileConv: "image-conv" });
      return Promise.resolve([]);
    });
    await act(async () => {
      const event = new Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(event, "clipboardData", {
        value: { files: [file("private.png")], items: [] },
      });
      container.querySelector("textarea")!.dispatchEvent(event);
    });
    const send = () =>
      container
        .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
        .click();
    await act(async () => send());
    expect(
      container.querySelector('[data-testid="composer-pending-files"]'),
    ).toBeNull();
    const intents = Object.values(useChat.getState().intents);
    expect(intents).toHaveLength(1);
    expect(intents[0].message.failed).toBe(true);
    expect(intents[0].payload).toMatchObject({
      path: "/tmp/private.png",
      protection: "authenticate",
    });
    await act(async () => send());
    expect(attempts).toBe(1);
    expect(container.textContent).toContain(
      "Text & images · hides after 30 seconds",
    );
    if (laterEdit === "mode") {
      document.body.append(container);
      await act(async () =>
        container
          .querySelector<HTMLButtonElement>(
            '[data-testid="composer-protection"]',
          )!
          .click(),
      );
      await act(async () =>
        document
          .querySelector<HTMLButtonElement>(
            '[data-testid="composer-protection-click"]',
          )!
          .click(),
      );
    }
    if (laterEdit === "attachment" || laterEdit === "replacement") {
      const paste = async (name: string) =>
        act(async () => {
          const event = new Event("paste", { bubbles: true, cancelable: true });
          Object.defineProperty(event, "clipboardData", {
            value: { files: [file(name)], items: [] },
          });
          container.querySelector("textarea")!.dispatchEvent(event);
        });
      await paste("new.png");
      if (laterEdit === "replacement") {
        await act(async () =>
          container
            .querySelector<HTMLButtonElement>('[aria-label="Dismiss new.png"]')!
            .click(),
        );
        await paste("replacement.png");
      }
    }
    await act(async () =>
      useChat.getState().retry(intents[0].message.clientId!),
    );
    expect(composer.protectionReset?.applyIf()).toBe(
      laterEdit !== "none" ? undefined : true,
    );
    if (laterEdit === "none")
      expect(container.textContent).not.toContain(
        "Text & images · hides after 30 seconds",
      );
    else
      expect(container.textContent).toContain(
        "Text & images · hides after 30 seconds",
      );
    if (laterEdit === "mode")
      expect(container.textContent).toContain("Click to view");
    expect(attempts).toBe(2);
    expect(Object.values(useChat.getState().intents)).toHaveLength(1);
    expect(Object.values(useChat.getState().intents)[0].payload).toMatchObject({
      path: "",
      protection: "authenticate",
    });
  },
);

it("does not retarget a refused remainder into a replacement conversation", async () => {
  await act(async () => composer.onProtectionChange!("authenticate"));
  const admitImage = vi.fn().mockResolvedValue("refused");
  useChat.setState({ admitImage });
  native.invoke.mockImplementation((command: string) =>
    command === "write_temp_file"
      ? Promise.resolve("/tmp/private.png")
      : Promise.resolve(false),
  );
  const image = file("private.png");
  await act(async () => {
    await expect(composer.onPasteFiles!([image])).rejects.toThrow(
      "Image not accepted",
    );
  });
  expect(native.invoke).toHaveBeenCalledWith("delete_temp_file", {
    path: "/tmp/private.png",
  });
  await switchConversation();
  await act(async () => composer.onPasteFiles!([image]));
  expect(admitImage).toHaveBeenCalledTimes(1);
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it("consumes unchanged protection after a locally refused partial batch finally succeeds", async () => {
  const { Composer: ActualComposer } =
    await vi.importActual<typeof import("../Composer")>("../Composer");
  await act(async () => composer.onProtectionChange!("authenticate"));
  useChat.setState({
    admitImage: originalAdmitImage,
    intents: {},
    reload: vi.fn().mockResolvedValue(true),
  });
  actualComposer = ActualComposer;
  await act(async () => root.render(createElement(ConversationView)));
  let stages = 0;
  native.invoke.mockImplementation(
    (command: string, args: { name?: string }) => {
      if (command === "write_temp_file")
        return ++stages === 2
          ? Promise.reject(new Error("disk full"))
          : Promise.resolve(`/tmp/${args.name}`);
      if (command === "owner_enqueue_file")
        return Promise.resolve({
          id: `image-${stages}`,
          fileConv: `conv-${stages}`,
        });
      return Promise.resolve([]);
    },
  );
  await act(async () => {
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", {
      value: { files: [file("one.png"), file("two.png")], items: [] },
    });
    container.querySelector("textarea")!.dispatchEvent(event);
  });
  const send = () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click();
  await act(async () => send());
  expect(
    container.querySelector('[data-testid="composer-pending-files"]')!
      .textContent,
  ).toContain("two.png");
  expect(container.textContent).toContain(
    "Text & images · hides after 30 seconds",
  );
  await act(async () => send());
  expect(
    container.querySelector('[data-testid="composer-pending-files"]'),
  ).toBeNull();
  expect(container.textContent).not.toContain(
    "Text & images · hides after 30 seconds",
  );
  expect(
    native.invoke.mock.calls
      .filter(([command]) => command === "owner_enqueue_file")
      .map(([, args]) => args.protection),
  ).toEqual(["authenticate", "authenticate"]);
});

it.each(["drop", "picker"])(
  "keeps failed %s protection and consumes it only when its bubble retry succeeds",
  async (source) => {
    const { Composer: ActualComposer } =
      await vi.importActual<typeof import("../Composer")>("../Composer");
    await act(async () => composer.onProtectionChange!("authenticate"));
    useChat.setState({
      admitImage: originalAdmitImage,
      retry: originalRetry,
      intents: {},
      reload: vi.fn().mockResolvedValue(true),
    });
    actualComposer = ActualComposer;
    await act(async () => root.render(createElement(ConversationView)));
    let attempts = 0;
    native.invoke.mockImplementation((command: string) =>
      command === "owner_enqueue_file"
        ? ++attempts === 1
          ? Promise.reject(new Error("offline"))
          : Promise.resolve({ id: "accepted-image", fileConv: "image-conv" })
        : Promise.resolve([]),
    );
    native.open.mockResolvedValue("/native/private.png");
    await act(async () => {
      if (source === "drop")
        native.drop!({
          payload: { type: "drop", paths: ["/native/private.png"] },
        });
      else await composer.onImage!();
    });
    const intent = Object.values(useChat.getState().intents)[0];
    expect(intent.message.failed).toBe(true);
    expect(container.textContent).toContain(
      "Text & images · hides after 30 seconds",
    );
    expect(composer.protectionReset).toBeUndefined();
    await act(async () => useChat.getState().retry(intent.message.clientId!));
    expect(attempts).toBe(2);
    expect(container.textContent).not.toContain(
      "Text & images · hides after 30 seconds",
    );
  },
);

it("keeps rejected protected text solely in its quoted composer draft before a single successful resubmission", async () => {
  const { Composer: ActualComposer } =
    await vi.importActual<typeof import("../Composer")>("../Composer");
  const quoted = {
    id: "quoted",
    text: "Original message",
    fromMe: false,
    who: "peer",
    wallClock: 1,
    replyTo: null,
  };
  useChat.setState({
    admitTextAndWait: originalAdmitTextAndWait,
    intents: {},
    messages: { "account:alice": [quoted] },
    reload: vi.fn().mockResolvedValue(true),
  });
  await act(async () => composer.onProtectionChange!("authenticate"));
  actualComposer = ActualComposer;
  await act(async () => root.render(createElement(ConversationView)));
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="reply-quoted"]')!
      .click(),
  );
  await act(async () => {
    const input = container.querySelector("textarea")!;
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(input, "Private reply");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  let attempts = 0;
  native.invoke.mockImplementation((command: string) =>
    command === "owner_enqueue_text"
      ? ++attempts === 1
        ? Promise.reject(new Error("offline"))
        : Promise.resolve("accepted-text")
      : Promise.resolve([]),
  );
  const send = () =>
    container
      .querySelector<HTMLButtonElement>('[data-testid="composer-send"]')!
      .click();
  await act(async () => send());
  expect(container.querySelector("textarea")!.value).toBe("Private reply");
  expect(composer.replyTo?.id).toBe("quoted");
  expect(container.textContent).toContain(
    "Text & images · hides after 30 seconds",
  );
  expect(Object.values(useChat.getState().intents)).toHaveLength(0);
  expect(useChat.getState().messages["account:alice"]).toEqual([quoted]);
  await act(async () => send());
  expect(attempts).toBe(2);
  expect(container.querySelector("textarea")!.value).toBe("");
  expect(composer.replyTo).toBeNull();
  expect(container.textContent).not.toContain(
    "Text & images · hides after 30 seconds",
  );
  expect(
    useChat
      .getState()
      .messages["account:alice"].filter((message) => message.fromMe),
  ).toHaveLength(1);
  expect(
    native.invoke.mock.calls
      .filter(([command]) => command === "owner_enqueue_text")
      .map(([, args]) => ({
        text: args.text,
        replyTo: args.replyTo,
        protection: args.protection,
      })),
  ).toEqual([
    { text: "Private reply", replyTo: "quoted", protection: "authenticate" },
    { text: "Private reply", replyTo: "quoted", protection: "authenticate" },
  ]);
});

it("rejects native drop acquisition and overlay for a shared reference with no other read-only flags", async () => {
  let intake!: ReturnType<typeof useAttachmentIntake>;
  function Probe() {
    intake = useAttachmentIntake(i18n.t);
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  await act(async () =>
    useChat.setState({
      active: { kind: "channel", id: "team", name: "Team" },
      channels: [
        {
          channel_id: "team",
          name: "Team",
          owner: "owner",
          member_count: 1,
          shared_reference: true,
          archived: false,
          dissolved: false,
          left: false,
          pending_invite: false,
        },
      ],
    }),
  );
  await act(async () => native.drop!({ payload: { type: "over", paths: [] } }));
  expect(intake.dragOver).toBe(false);
  await act(async () =>
    native.drop!({ payload: { type: "drop", paths: ["/native/file"] } }),
  );
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it("cleans up a staged attachment if the same group becomes a shared reference before handoff", async () => {
  let intake!: ReturnType<typeof useAttachmentIntake>;
  function Probe() {
    intake = useAttachmentIntake(i18n.t);
    return null;
  }
  await act(async () => root.render(createElement(Probe)));
  const channel = {
    channel_id: "team",
    name: "Team",
    owner: "owner",
    member_count: 1,
  };
  await act(async () =>
    useChat.setState({
      active: { kind: "channel", id: "team", name: "Team" },
      channels: [channel],
    }),
  );
  const staged = deferred<string>();
  native.invoke.mockImplementation((command: string) =>
    command === "write_temp_file" ? staged.promise : Promise.resolve(),
  );
  let sending!: Promise<void>;
  await act(async () => {
    sending = intake.pasteFiles([file()]);
  });
  expect(native.invoke).toHaveBeenCalledWith(
    "write_temp_file",
    expect.any(Object),
  );
  await act(async () =>
    useChat.setState({ channels: [{ ...channel, shared_reference: true }] }),
  );
  await act(async () => {
    staged.resolve("/tmp/reference-photo.png");
    await sending;
  });
  expect(native.invoke).toHaveBeenCalledWith("delete_temp_file", {
    path: "/tmp/reference-photo.png",
  });
  expect(useChat.getState().sendFile).not.toHaveBeenCalled();
});

it.each(["picker", "staging"] as const)(
  "retires pending %s acquisition across same-group reference and restoration",
  async (kind) => {
    let intake!: ReturnType<typeof useAttachmentIntake>;
    function Probe() {
      intake = useAttachmentIntake(i18n.t);
      return null;
    }
    await act(async () => root.render(createElement(Probe)));
    const channel = {
      channel_id: "team",
      name: "Team",
      owner: "owner",
      member_count: 1,
    };
    await act(async () =>
      useChat.setState({
        active: { kind: "channel", id: "team", name: "Team" },
        channels: [channel],
      }),
    );
    const prepared = deferred<string>();
    native.open.mockReturnValue(prepared.promise);
    native.invoke.mockImplementation((command: string) =>
      command === "write_temp_file" ? prepared.promise : Promise.resolve(),
    );
    let sending!: Promise<void>;
    await act(async () => {
      sending =
        kind === "picker" ? intake.attachFile() : intake.pasteFiles([file()]);
    });
    await act(async () =>
      useChat.setState({ channels: [{ ...channel, shared_reference: true }] }),
    );
    await act(async () => useChat.setState({ channels: [channel] }));
    await act(async () => {
      prepared.resolve("/tmp/retired-reference.png");
      await sending;
    });
    expect(useChat.getState().sendFile).not.toHaveBeenCalled();
    if (kind === "staging")
      expect(native.invoke).toHaveBeenCalledWith("delete_temp_file", {
        path: "/tmp/retired-reference.png",
      });
  },
);

it.each([
  "shared_reference",
  "archived",
  "dissolved",
  "left",
  "pending_invite",
] as const)(
  "clears screenshot and native drag state immediately when the active group becomes %s",
  async (flag) => {
    let intake!: ReturnType<typeof useAttachmentIntake>;
    function Probe() {
      intake = useAttachmentIntake(i18n.t);
      return null;
    }
    await act(async () => root.render(createElement(Probe)));
    const channel = {
      channel_id: "team",
      name: "Team",
      owner: "owner",
      member_count: 1,
    };
    await act(async () =>
      useChat.setState({
        active: { kind: "channel", id: "team", name: "Team" },
        channels: [channel],
      }),
    );
    native.invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "capture_screen" ? [1, 2] : false),
    );
    await act(async () => intake.captureScreenshot(false));
    await act(async () =>
      native.drop!({ payload: { type: "over", paths: [] } }),
    );
    expect(intake.screenshotBytes).not.toBeNull();
    expect(intake.dragOver).toBe(true);
    await act(async () =>
      useChat.setState({ channels: [{ ...channel, [flag]: true }] }),
    );
    expect(intake.screenshotBytes).toBeNull();
    expect(intake.dragOver).toBe(false);
    await act(async () => useChat.setState({ channels: [channel] }));
    expect(intake.screenshotBytes).toBeNull();
    expect(intake.dragOver).toBe(false);
  },
);

it.each(["member", "owner"] as const)(
  "allows certified reference-only %s File intake through existing handoff",
  async (role) => {
    let intake!: ReturnType<typeof useAttachmentIntake>;
    function Probe() {
      intake = useAttachmentIntake(i18n.t);
      return null;
    }
    await act(async () => root.render(createElement(Probe)));
    await act(async () =>
      useChat.setState({
        active: { kind: "channel", id: "team", name: "Team" },
        channels: [
          {
            channel_id: "team",
            name: "Team",
            owner: "sibling",
            member_count: 1,
            shared_reference: true,
            account_access: {
              account_id: useChat.getState().myAccountId,
              role,
              execution: { kind: "waiting_for_device" },
            },
          },
        ],
      }),
    );
    await act(async () =>
      native.drop!({ payload: { type: "over", paths: [] } }),
    );
    expect(intake.dragOver).toBe(true);
    await act(async () =>
      native.drop!({ payload: { type: "drop", paths: ["/native/file"] } }),
    );
    expect(useChat.getState().sendFile).toHaveBeenCalledWith(
      "/native/file",
      false,
    );
  },
);
