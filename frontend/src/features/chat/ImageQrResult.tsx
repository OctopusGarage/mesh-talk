import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { displayIdentity } from "@/lib/displayIdentity";
import { rosterProjection } from "@/lib/rosterProjection";
import { captureChatOwnership, useChat } from "@/store/chat";

const MAX_SCAN_EDGE = 1536;

type ScanResult =
  | { kind: "text"; value: string }
  | { kind: "account"; value: string }
  | { kind: "link"; value: string }
  | { kind: "none" | "error" };

function classify(value: string): ScanResult {
  if (/^[0-9a-f]{32}$/i.test(value))
    return { kind: "account", value: value.toLowerCase() };
  if (
    /^https?:\/\//i.test(value) &&
    !/[\\\s]/u.test(value) &&
    !Array.from(value).some((character) => {
      const code = character.charCodeAt(0);
      return code < 32 || code === 127;
    })
  ) {
    try {
      const url = new URL(value);
      if (url.hostname && !url.username && !url.password)
        return { kind: "link", value: url.href };
    } catch {
      return { kind: "text", value };
    }
  }
  return { kind: "text", value };
}

/** Mounted only by the scan action. Its worker and result belong to one image. */
export function ImageQrResult({
  url,
  onClose,
  onOpenChat,
}: {
  url: string | null;
  onClose: () => void;
  onOpenChat: () => void;
}) {
  const { t } = useTranslation();
  const [{ current }] = useState(captureChatOwnership);
  const [result, setResult] = useState<ScanResult>();
  const [actionFailed, setActionFailed] = useState(false);
  const peers = useChat((state) => state.peers);
  const myAccountId = useChat((state) => state.myAccountId);
  const canChat =
    result?.kind === "account" &&
    (result.value === myAccountId ||
      !!rosterProjection(peers).contactForAccount(result.value));

  useEffect(() => {
    if (!current()) return;
    let cancelled = false;
    let worker: Worker | undefined;
    const finish = (next: ScanResult) => {
      clearTimeout(timeout);
      worker?.terminate();
      worker = undefined;
      if (!cancelled && current()) setResult(next);
      cancelled = true;
    };
    const timeout = window.setTimeout(() => finish({ kind: "error" }), 15_000);
    const scan = async () => {
      if (!url) return;
      const image = new Image();
      image.src = url;
      await image.decode();
      if (cancelled || !current()) return;
      const scale = Math.min(
        1,
        MAX_SCAN_EDGE / Math.max(image.naturalWidth, image.naturalHeight),
      );
      const width = Math.max(1, Math.floor(image.naturalWidth * scale));
      const height = Math.max(1, Math.floor(image.naturalHeight * scale));
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d");
      if (!context) throw new Error("Image pixels unavailable");
      // Transparent pixels have the same white backing as the app's QR cards.
      context.fillStyle = "#fff";
      context.fillRect(0, 0, width, height);
      context.drawImage(image, 0, 0, width, height);
      const pixels = context.getImageData(0, 0, width, height).data;
      worker = new Worker(new URL("./imageQr.worker.ts", import.meta.url), {
        type: "module",
      });
      worker.onmessage = ({ data }: MessageEvent<string | null>) =>
        finish(data === null ? { kind: "none" } : classify(data));
      worker.onerror = () => finish({ kind: "error" });
      worker.postMessage({ pixels, width, height }, [pixels.buffer]);
    };
    void scan().catch(() => finish({ kind: "error" }));
    return () => {
      cancelled = true;
      clearTimeout(timeout);
      worker?.terminate();
    };
  }, [url, current]);

  const openChat = () => {
    if (!current() || result?.kind !== "account") return;
    const state = useChat.getState();
    if (
      result.value !== state.myAccountId &&
      !rosterProjection(state.peers).contactForAccount(result.value)
    )
      return;
    const identity = displayIdentity({
      accountId: result.value,
      accounts: state.accounts,
      peers: state.peers,
      favorites: state.favorites,
    });
    onOpenChat();
    void state.open({ kind: "account", id: result.value, name: identity.name });
  };

  return (
    <section
      aria-label={t("files.recognizeQr")}
      className="absolute inset-x-3 bottom-20 z-10 mx-auto max-h-[55%] max-w-md overflow-auto rounded-lg border border-border bg-popover p-4 text-popover-foreground shadow-lg"
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        if (event.key !== "Tab") event.stopPropagation();
      }}
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="font-semibold">{t("files.recognizeQr")}</h2>
        <Button variant="ghost" size="sm" onClick={onClose}>
          {t("common.close")}
        </Button>
      </div>
      <div role="status" className="text-sm">
        {!result && t("files.qrScanning")}
        {result?.kind === "none" && t("files.qrNotFound")}
        {result?.kind === "error" && t("files.qrUnreadable")}
        {result && "value" in result && (
          <p
            className="select-text whitespace-pre-wrap [overflow-wrap:anywhere]"
            dir="ltr"
          >
            {result.value}
          </p>
        )}
      </div>
      {actionFailed && (
        <p role="alert" className="text-sm">
          {t("files.qrOpenFailed")}
        </p>
      )}
      {result?.kind === "link" && (
        <Button
          size="sm"
          className="mt-3"
          onClick={() => {
            if (!current()) return;
            void openUrl(result.value).catch(() => {
              if (current()) setActionFailed(true);
            });
          }}
        >
          {t("files.qrOpenLink")}
        </Button>
      )}
      {canChat && (
        <Button size="sm" className="mt-3" onClick={openChat}>
          {t("files.qrOpenChat")}
        </Button>
      )}
    </section>
  );
}
