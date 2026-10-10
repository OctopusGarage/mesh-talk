import { useCallback, useEffect, useRef, useState } from "react";
import { open as openDialog, save } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import { Download, ExternalLink, FolderOpen, Search, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { IdentityGlyph } from "@/components/identity";
import { chat, settings as settingsApi } from "@/lib/api";
import { defaultSavePath, effectiveDownloadDir } from "@/lib/download";
import {
  attachmentLabel,
  isDirectoryAttachment,
} from "@/lib/directoryAttachment";
import { rememberSavedDownload, useSavedDownloads } from "@/lib/savedDownloads";
import { errorMessage } from "@/lib/error";
import { humanSize } from "@/lib/format";
import { useChat, captureChatOwnership } from "@/store/chat";
import {
  useFileAvailability,
  watchFileAvailability,
} from "@/store/fileAvailability";
import { TransferBar } from "./TransferBar";
import { ReceiveProgress } from "./ReceiveProgress";
import { fileGlyph } from "./mediaFile";

export function FilesTray({ navigation = false }: { navigation?: boolean }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  // "Received files" is ATTACHMENTS only. A file sent via the media button is shown inline +
  // auto-saved, so it never appears here — decided by the sender's INTENT (manifest kind),
  // not the file extension (a .mov sent via the attach button DOES belong here).
  const incoming = useChat((s) => s.incomingFiles);
  const files = incoming.filter((f) => !f.media);
  const [query, setQuery] = useState("");
  const [savingFile, setSavingFile] = useState<string | null>(null);
  const visibleFiles = files.filter((f) =>
    `${f.name} ${f.fromName}`
      .toLocaleLowerCase()
      .includes(query.trim().toLocaleLowerCase()),
  );
  // Saved destinations survive restart, so rows reveal the local copy after chunks are pruned.
  const savedPaths = useSavedDownloads();
  const visibleFileKeys = visibleFiles
    .filter((file) => !savedPaths[file.fileConv])
    .map((file) => file.fileConv)
    .join("|");
  const statuses = useFileAvailability((s) => s.statuses);
  useEffect(() => {
    if (!open) return;
    const stop = visibleFileKeys
      .split("|")
      .filter(Boolean)
      .map((fileConv) => watchFileAvailability(fileConv));
    return () => stop.forEach((unwatch) => unwatch());
  }, [open, visibleFileKeys]);
  const dismissFile = useChat((s) => s.dismissFile);
  const setError = useChat((s) => s.setError);

  // Remembered default download folder ("" = use the OS Downloads folder). Loaded on open, kept as
  // a primitive (memory: zustand selectors returning fresh objects can black-screen).
  const [downloadDir, setDownloadDir] = useState("");
  const [folderLoaded, setFolderLoaded] = useState(false);
  const [folderError, setFolderError] = useState<string | null>(null);
  const [folderOpenError, setFolderOpenError] = useState<string | null>(null);
  const [openingDir, setOpeningDir] = useState(false);
  const folderRequest = useRef(0);
  const loadDir = useCallback(async () => {
    const request = ++folderRequest.current;
    setFolderLoaded(false);
    setFolderError(null);
    setFolderOpenError(null);
    try {
      const settings = await settingsApi.get();
      if (request !== folderRequest.current) return;
      setDownloadDir(settings.download_dir);
      setFolderLoaded(true);
    } catch (e) {
      if (request === folderRequest.current) setFolderError(errorMessage(e));
    }
  }, []);
  useEffect(() => {
    if (open) void loadDir();
  }, [open, loadDir]);

  // Pick (and persist) the remembered default download folder.
  const chooseDir = async () => {
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (lease.current() && typeof dir === "string") {
        if (!lease.current()) return;
        await settingsApi.update({ download_dir: dir });
        if (lease.current()) {
          folderRequest.current++;
          setDownloadDir(dir);
          setFolderLoaded(true);
          setFolderError(null);
          setFolderOpenError(null);
        }
      }
    } catch (e) {
      if (lease.current())
        setError(t("files.couldntSave", { error: errorMessage(e) }));
    }
  };

  const openDownloadDir = async () => {
    const lease = captureChatOwnership();
    if (!lease.current() || !folderLoaded || folderError || openingDir) return;
    setOpeningDir(true);
    setFolderOpenError(null);
    try {
      const dir = downloadDir || (await chat.defaultDownloadDir());
      if (!lease.current()) return;
      if (!dir) {
        setFolderOpenError(t("files.folderUnknown"));
        return;
      }
      await openPath(dir);
    } catch (e) {
      if (lease.current())
        setFolderOpenError(t("files.couldntOpen", { error: errorMessage(e) }));
    } finally {
      setOpeningDir(false);
    }
  };

  // Save into the effective download folder with no prompt: the folder the user chose, else
  // the OS Downloads folder (the common default). Only falls back to a Save-as dialog if no
  // folder is resolvable at all.
  const saveToDefault = async (
    fileConv: string,
    name: string,
    mime: string,
  ) => {
    const lease = captureChatOwnership();
    if (
      !lease.current() ||
      savingFile !== null ||
      !useFileAvailability.getState().statuses[fileConv]?.ready
    )
      return;
    try {
      setSavingFile(fileConv);
      const dir = await effectiveDownloadDir();
      if (!lease.current()) return;
      if (dir) {
        const path = await chat.saveFileToDir(fileConv, dir);
        if (lease.current()) rememberSavedDownload(fileConv, path);
        return;
      }
      const dest = isDirectoryAttachment(mime)
        ? await openDialog({ directory: true })
        : await save({ defaultPath: name });
      if (lease.current() && typeof dest === "string") {
        const path = isDirectoryAttachment(mime)
          ? await chat.saveFileToDir(fileConv, dest)
          : (await chat.saveFile(fileConv, dest), dest);
        if (lease.current()) rememberSavedDownload(fileConv, path);
      }
    } catch (e) {
      if (lease.current()) handleSaveError(e);
    } finally {
      setSavingFile(null);
    }
  };

  // Always-prompt "Save as…" override; the dialog opens at the Downloads folder.
  const saveAs = async (fileConv: string, name: string, mime: string) => {
    const lease = captureChatOwnership();
    if (
      !lease.current() ||
      savingFile !== null ||
      !useFileAvailability.getState().statuses[fileConv]?.ready
    )
      return;
    try {
      setSavingFile(fileConv);
      const defaultPath = await defaultSavePath(name);
      if (!lease.current()) return;
      const dest = isDirectoryAttachment(mime)
        ? await openDialog({ directory: true })
        : await save({ defaultPath });
      if (lease.current() && typeof dest === "string") {
        const path = isDirectoryAttachment(mime)
          ? await chat.saveFileToDir(fileConv, dest)
          : (await chat.saveFile(fileConv, dest), dest);
        if (lease.current()) rememberSavedDownload(fileConv, path);
      }
    } catch (e) {
      if (lease.current()) handleSaveError(e);
    } finally {
      setSavingFile(null);
    }
  };

  // A save failure handler that recognizes the already-downloaded case: chunks are pruned
  // after a successful save, so a later save attempt fails with "file incomplete" — that
  // means it was already downloaded (and reclaimed), not a real error.
  const handleSaveError = (e: unknown) => {
    const msg = errorMessage(e);
    setError(
      /incomplete/i.test(msg)
        ? t("files.alreadyDownloaded")
        : t("files.couldntSave", { error: msg }),
    );
  };

  const reveal = async (path: string) => {
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    try {
      await revealItemInDir(path);
    } catch (e) {
      if (lease.current())
        setError(t("files.couldntOpen", { error: errorMessage(e) }));
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          data-testid="sidebar-action-files"
          title={t("files.received")}
          aria-label={t("files.received")}
          className={
            navigation
              ? "relative h-10 w-full justify-start gap-3 px-3 text-muted-foreground hover:text-foreground"
              : "relative h-9 w-9 shrink-0 p-0 text-muted-foreground"
          }
        >
          <Download className="h-4 w-4 shrink-0" />
          <span
            className={navigation ? "sidebar-tool-label truncate" : "sr-only"}
          >
            {t("files.received")}
          </span>
          {files.length > 0 && (
            <Badge
              className={
                navigation
                  ? "ml-auto h-5 min-w-5 justify-center bg-muted px-1.5 text-muted-foreground"
                  : "absolute -right-0.5 -top-0.5 h-4 min-w-4 justify-center bg-primary px-1 text-primary-foreground"
              }
            >
              {files.length}
            </Badge>
          )}
        </Button>
      </DialogTrigger>
      <DialogContent
        className="flex max-h-[min(680px,calc(100vh-2rem))] max-w-2xl flex-col gap-0 overflow-hidden p-0"
        data-testid="files-tray"
      >
        <DialogHeader className="border-b px-6 pb-5 pt-6 pr-14">
          <DialogTitle>{t("files.received")}</DialogTitle>
          <DialogDescription>{t("files.description")}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-wrap items-center gap-3 border-b px-6 py-3">
          <div className="relative min-w-40 flex-1">
            <Search
              className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
              aria-hidden="true"
            />
            <Input
              data-testid="files-search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("files.searchPlaceholder")}
              aria-label={t("files.searchPlaceholder")}
              className="h-9 pl-9"
            />
          </div>
          <button
            type="button"
            onClick={() => void chooseDir()}
            title={t("files.chooseFolder")}
            className="flex min-h-9 min-w-0 max-w-full items-center gap-2 rounded-md px-2 text-xs text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <FolderOpen className="h-4 w-4 shrink-0" />
            <span className="max-w-44 truncate">
              {folderError
                ? t("files.folderUnknown")
                : folderLoaded
                  ? downloadDir || t("files.noDefaultFolder")
                  : t("common.loading")}
            </span>
          </button>
          <Button
            type="button"
            size="sm"
            variant="secondary"
            data-testid="files-open-folder"
            disabled={!folderLoaded || !!folderError || openingDir}
            onClick={() => void openDownloadDir()}
            className="h-9 shrink-0 gap-1.5"
          >
            <ExternalLink className="h-3.5 w-3.5" aria-hidden="true" />
            {t("files.openFolder")}
          </Button>
        </div>
        {folderError && (
          <div
            role="alert"
            data-testid="files-folder-error"
            className="flex items-center justify-between gap-3 border-b px-6 py-2 text-xs text-destructive"
          >
            <span>{t("files.folderLoadFailed", { error: folderError })}</span>
            <button
              type="button"
              onClick={() => void loadDir()}
              className="shrink-0 font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("contactVisibility.retry")}
            </button>
          </div>
        )}
        {folderOpenError && (
          <div
            role="alert"
            data-testid="files-open-folder-error"
            className="border-b px-6 py-2 text-xs text-destructive"
          >
            {folderOpenError}
          </div>
        )}
        {files.length === 0 ? (
          <div className="flex min-h-48 flex-col items-center justify-center gap-3 px-6 py-8 text-center">
            <Download className="h-8 w-8 text-muted-foreground/40" />
            <p className="text-sm text-muted-foreground">
              {t("files.nothingReceived")}
            </p>
          </div>
        ) : visibleFiles.length === 0 ? (
          <div className="flex min-h-40 items-center justify-center px-6 py-8 text-center text-sm text-muted-foreground">
            {t("files.noMatches")}
          </div>
        ) : (
          <div className="min-h-0 overflow-y-auto px-4 py-2">
            {visibleFiles.map((f) => (
              <div
                key={f.fileConv}
                className="rounded-lg px-2 py-3 hover:bg-accent/50"
              >
                <div className="flex items-center gap-2">
                  {fileGlyph(f.name, f.mime)}
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">
                      {attachmentLabel(f.name, f.mime)}
                    </div>
                    <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                      <IdentityGlyph
                        seed={f.fromName}
                        size={14}
                        className="shrink-0"
                        title={f.fromName}
                      />
                      <span className="truncate">{f.fromName}</span>
                      <span aria-hidden>·</span>
                      <span className="shrink-0 font-mono">
                        {humanSize(f.size)}
                      </span>
                    </div>
                    {savedPaths[f.fileConv] && (
                      <div
                        className="truncate text-[11px] text-muted-foreground"
                        title={savedPaths[f.fileConv]}
                      >
                        {t("files.savedTo", { path: savedPaths[f.fileConv] })}
                      </div>
                    )}
                  </div>
                  {savedPaths[f.fileConv] ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="min-h-9"
                      // Title shows WHERE it was saved; click reveals it in the file manager.
                      title={t("files.savedTo", {
                        path: savedPaths[f.fileConv],
                      })}
                      onClick={() => void reveal(savedPaths[f.fileConv])}
                    >
                      <FolderOpen className="h-3.5 w-3.5" />
                      {t("files.reveal")}
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="secondary"
                      className="min-h-9"
                      disabled={
                        !statuses[f.fileConv]?.ready || savingFile !== null
                      }
                      onClick={() =>
                        void saveToDefault(f.fileConv, f.name, f.mime)
                      }
                    >
                      {t("common.save")}
                    </Button>
                  )}
                  <button
                    onClick={() => dismissFile(f.fileConv)}
                    type="button"
                    title={t("common.dismiss")}
                    aria-label={t("common.dismiss")}
                    className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
                {!savedPaths[f.fileConv] && (
                  <button
                    type="button"
                    disabled={
                      !statuses[f.fileConv]?.ready || savingFile !== null
                    }
                    onClick={() => void saveAs(f.fileConv, f.name, f.mime)}
                    className="mt-1 pl-6 text-xs text-muted-foreground hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {t("files.saveAs")}
                  </button>
                )}
                {!savedPaths[f.fileConv] && (
                  <ReceiveProgress status={statuses[f.fileConv]} />
                )}
                <TransferBar transferKey={f.fileConv} />
              </div>
            ))}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
