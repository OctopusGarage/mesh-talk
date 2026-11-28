import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import type { FavoriteSource } from "@/lib/api/messageFavorites";
import { useFavoriteClock } from "./favorites/useFavoriteClock";
import { captureRuntimeOwner } from "@/store/ownership";

export function LanReviewDialog({
  source,
  expiresAt,
  onClose,
}: {
  source: FavoriteSource;
  expiresAt: number | null | undefined;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [lease] = useState(captureRuntimeOwner);
  const now = useFavoriteClock([expiresAt], false, lease.current);
  const expired = expiresAt != null && expiresAt <= now;
  const mounted = useRef(false);
  const activeId = useRef<string | null>(null);
  const inFlight = useRef(false);
  const [preview, setPreview] = useState<string | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<
    "previewFailed" | "startFailed" | "revokeFailed" | "copyFailed" | null
  >(null);

  useEffect(() => {
    mounted.current = true;
    let cancelled = false;
    const { owner } = lease;
    if (expired) {
      setPreview(null);
    } else if (owner && lease.current()) {
      void invoke<string>("owner_preview_lan_review", { owner, source }).then(
        (text) => {
          if (!cancelled && lease.current()) setPreview(text);
        },
        () => {
          if (!cancelled && lease.current()) setError("previewFailed");
        },
      );
    }
    return () => {
      cancelled = true;
      mounted.current = false;
    };
  }, [expired, lease, source]);

  useEffect(() => {
    const { owner, current: isCurrent } = lease;
    return () => {
      const grantId = activeId.current;
      if (grantId && owner && isCurrent()) {
        void invoke("owner_revoke_lan_review", { owner, grantId }).catch(
          () => {},
        );
      }
    };
  }, [lease]);

  async function updateLink() {
    const { owner } = lease;
    if (
      inFlight.current ||
      !owner ||
      !lease.current() ||
      (!url && (expired || preview === null))
    )
      return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      if (url) {
        await invoke("owner_revoke_lan_review", {
          owner,
          grantId: activeId.current,
        });
        activeId.current = null;
        if (lease.current() && mounted.current) onClose();
      } else {
        const result = await invoke<{ id: string; url: string }>(
          "owner_start_lan_review",
          {
            owner,
            source,
            preview,
          },
        );
        if (lease.current() && mounted.current) {
          activeId.current = result.id;
          setUrl(result.url);
          setPreview(null);
        } else if (lease.current()) {
          await invoke("owner_revoke_lan_review", {
            owner,
            grantId: result.id,
          });
        }
      }
    } catch {
      if (lease.current() && mounted.current) {
        setError(url ? "revokeFailed" : "startFailed");
      }
    } finally {
      inFlight.current = false;
      if (lease.current() && mounted.current) setBusy(false);
    }
  }

  async function copyLink() {
    if (!url || !lease.current()) return;
    try {
      await navigator.clipboard.writeText(url);
      if (lease.current() && mounted.current) {
        setCopied(true);
        setError(null);
      }
    } catch {
      if (lease.current() && mounted.current) setError("copyFailed");
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (open || inFlight.current) return;
        if (url) void updateLink();
        else onClose();
      }}
    >
      <DialogContent data-testid="lan-review-dialog">
        <DialogHeader>
          <DialogTitle>{t("lanReview.title")}</DialogTitle>
          <DialogDescription>
            {t(url ? "lanReview.activeHint" : "lanReview.exposure")}
          </DialogDescription>
        </DialogHeader>
        {url ? (
          <Input
            aria-label={t("lanReview.url")}
            readOnly
            value={url}
            onFocus={(event) => event.currentTarget.select()}
          />
        ) : expired ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("lanReview.previewFailed")}
          </p>
        ) : preview !== null ? (
          <p className="max-h-[45dvh] overflow-y-auto whitespace-pre-wrap rounded-md border p-3 text-sm [overflow-wrap:anywhere]">
            {preview}
          </p>
        ) : !error ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("lanReview.loading")}
          </p>
        ) : null}
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {t(`lanReview.${error}`)}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          {url ? (
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => void copyLink()}
            >
              {t(copied ? "lanReview.copied" : "lanReview.copy")}
            </Button>
          ) : (
            <Button variant="ghost" disabled={busy} onClick={onClose}>
              {t("common.cancel")}
            </Button>
          )}
          <Button
            variant={url ? "destructive" : "default"}
            disabled={busy || (!url && (expired || preview === null))}
            onClick={() => void updateLink()}
          >
            {t(url ? "lanReview.revoke" : "lanReview.create")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
