import { onIncomingFileDismissed } from "@/lib/incomingFileDismissal";
import { create } from "zustand";
import { chat } from "@/lib/api";
import { effectiveDownloadDir } from "@/lib/download";
import { errorMessage } from "@/lib/error";
import {
  rememberSavedDownloadForOwner,
  savedDownloadPath,
} from "@/lib/savedDownloads";
import { captureChatOwnership, useChat, type IncomingFile } from "@/store/chat";
import {
  useFileAvailability,
  watchFileAvailability,
  refreshFileAvailability,
  markUnavailableFileError,
} from "@/store/fileAvailability";

// Native exports outlive React effects. Keep the owner lock and completion signal
// outside a worker so a ready/reconnect transition cannot export the same bytes twice.
const ownerOperations = new Map<string, string>();
const ownerListeners = new Map<string, Set<(completed?: string) => void>>();
const notifyOwner = (owner: string, completed?: string) => {
  ownerListeners.get(owner)?.forEach((listener) => listener(completed));
};

export const useAutomaticDownloads = create<{
  saving: string | null;
  failures: Record<string, string>;
  persistenceError: string | null;
}>(() => ({ saving: null, failures: {}, persistenceError: null }));

export function retryAutomaticDownload(fileConv: string) {
  useAutomaticDownloads.setState((state) => {
    const failures = { ...state.failures };
    delete failures[fileConv];
    return { failures };
  });
}

/** The chat session owns this worker, so closed trays and virtualized cards never
 * prevent accepted, verified attachments reaching the download directory. */
export function startAutomaticDownloads() {
  const lease = captureChatOwnership();
  let stopped = false;
  let busy = false;
  const completed = new Set<string>();
  const watches = new Map<string, () => void>();
  const current = () => !stopped && lease.current();
  const pending = new Map<string, IncomingFile>();
  const queueKey = `mesh-talk-pending-downloads:${lease.owner}`;
  try {
    const restored: unknown = JSON.parse(
      localStorage.getItem(queueKey) || "[]",
    );
    if (current() && Array.isArray(restored)) {
      const incoming = useChat.getState().incomingFiles;
      const known = new Set(incoming.map((file) => file.fileConv));
      const valid = restored.filter(
        (file): file is IncomingFile =>
          file &&
          typeof file.fileConv === "string" &&
          typeof file.name === "string" &&
          typeof file.mime === "string" &&
          typeof file.fromName === "string" &&
          typeof file.size === "number" &&
          typeof file.media === "boolean" &&
          !known.has(file.fileConv),
      );
      valid.forEach((file) => pending.set(file.fileConv, file));
      useChat.setState({
        incomingFiles: [...incoming, ...valid].slice(0, 300),
      });
    }
  } catch {
    // Storage can be unavailable; live transfers still download this session.
  }
  let persistedQueue: string | undefined;
  const persist = (files: IncomingFile[]) => {
    try {
      const serialized = JSON.stringify(files);
      if (serialized === persistedQueue) return;
      localStorage.setItem(queueKey, serialized);
      persistedQueue = serialized;
      if (current() && useAutomaticDownloads.getState().persistenceError)
        useAutomaticDownloads.setState({ persistenceError: null });
    } catch (error) {
      // Avoid retrying a rejected write on every poll. Keep the queue in memory
      // and make the loss of restart persistence visible for this session.
      persistedQueue = JSON.stringify(files);
      if (current())
        useAutomaticDownloads.setState({
          persistenceError: errorMessage(error),
        });
    }
  };
  useAutomaticDownloads.setState({
    saving: ownerOperations.get(lease.owner!) ?? null,
    failures: {},
    persistenceError: null,
  });

  const candidates = () => {
    for (const file of useChat.getState().incomingFiles) {
      if (!file.media || !file.mime.startsWith("image/"))
        pending.set(file.fileConv, file);
    }
    for (const fileConv of pending.keys()) {
      const status = useFileAvailability.getState().statuses[fileConv];
      if (
        completed.has(fileConv) ||
        savedDownloadPath(fileConv) ||
        status?.unavailable ||
        (status?.total === 0 && !status.ready && !status.consent)
      )
        pending.delete(fileConv);
    }
    return [...pending.values()];
  };
  const pump = async () => {
    if (!current()) return;
    const files = candidates();
    persist(files);
    const keys = new Set(files.map((file) => file.fileConv));
    for (const [key, stop] of watches) {
      if (!keys.has(key)) {
        stop();
        watches.delete(key);
      }
    }
    for (const key of keys) {
      if (!watches.has(key)) {
        // Register first: watching may synchronously publish availability.
        watches.set(key, () => {});
        watches.set(key, watchFileAvailability(key));
      }
    }
    if (busy || ownerOperations.has(lease.owner!)) return;
    const file = files.find(({ fileConv }) => {
      const status = useFileAvailability.getState().statuses[fileConv];
      return (
        status?.ready &&
        status.consent !== "pending" &&
        status.consent !== "declined" &&
        !useAutomaticDownloads.getState().failures[fileConv]
      );
    });
    if (!file) {
      if (useAutomaticDownloads.getState().saving !== null)
        useAutomaticDownloads.setState({ saving: null });
      return;
    }
    busy = true;
    ownerOperations.set(lease.owner!, file.fileConv);
    useAutomaticDownloads.setState({ saving: file.fileConv });
    try {
      const dir = await effectiveDownloadDir();
      if (!current() || completed.has(file.fileConv)) return;
      if (!dir)
        throw new Error(
          "Download folder is unavailable. Choose a download folder and retry.",
        );
      await refreshFileAvailability([file.fileConv]);
      if (!current() || completed.has(file.fileConv)) return;
      const status = useFileAvailability.getState().statuses[file.fileConv];
      if (
        !status?.ready ||
        status.consent === "pending" ||
        status.consent === "declined"
      )
        return;
      const path = await chat.saveFileToDir(file.fileConv, dir, lease.owner!);
      rememberSavedDownloadForOwner(lease.owner!, file.fileConv, path);
      completed.add(file.fileConv);
      pending.delete(file.fileConv);
      notifyOwner(lease.owner!, file.fileConv);
      if (!current()) {
        try {
          const queued: IncomingFile[] = JSON.parse(
            localStorage.getItem(queueKey) || "[]",
          );
          persist(queued.filter((entry) => entry.fileConv !== file.fileConv));
        } catch {
          /* Best effort. */
        }
        return;
      }
    } catch (error) {
      if (current() && markUnavailableFileError(file.fileConv, error)) return;
      if (current())
        useAutomaticDownloads.setState((state) => ({
          failures: { ...state.failures, [file.fileConv]: errorMessage(error) },
        }));
    } finally {
      if (current()) useAutomaticDownloads.setState({ saving: null });
      busy = false;
      ownerOperations.delete(lease.owner!);
      notifyOwner(lease.owner!);
    }
  };
  const onOwnerOperation = (fileConv?: string) => {
    if (!current()) return;
    if (fileConv) {
      completed.add(fileConv);
      pending.delete(fileConv);
    }
    void pump();
  };
  let listeners = ownerListeners.get(lease.owner!);
  if (!listeners) {
    listeners = new Set();
    ownerListeners.set(lease.owner!, listeners);
  }
  listeners.add(onOwnerOperation);
  const subscriptions = [
    onIncomingFileDismissed((fileConv) => {
      if (!current()) return;
      completed.add(fileConv);
      pending.delete(fileConv);
      void pump();
    }),
    useChat.subscribe(() => void pump()),
    useFileAvailability.subscribe(() => void pump()),
    useAutomaticDownloads.subscribe(() => {
      if (!busy) void pump();
    }),
  ];
  void pump();
  return () => {
    stopped = true;
    listeners.delete(onOwnerOperation);
    if (!listeners.size) ownerListeners.delete(lease.owner!);
    subscriptions.forEach((stop) => stop());
    watches.forEach((stop) => stop());
    useAutomaticDownloads.setState({
      saving: null,
      failures: {},
      persistenceError: null,
    });
  };
}
