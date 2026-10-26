import { createProtectedImageUrl } from "./protectedImage";
import { useEffect, useRef, useState } from "react";
import { LockKeyhole } from "lucide-react";
import { useTranslation } from "react-i18next";
import { chat } from "@/lib/api/chat";
import { isMacOverlay } from "@/lib/platform";
import { useAuth } from "@/store/auth";
import { useAppLock } from "@/store/appLock";
import { captureChatOwnership, useChat } from "@/store/chat";
import type { MessageProtection } from "@/lib/types";
import { createProtectedReveal } from "./protectedReveal";

export function ProtectedMessage({
  id,
  protection,
  image = false,
  mime,
}: {
  id: string | null;
  protection: MessageProtection;
  image?: boolean;
  mime?: string;
}) {
  const { t } = useTranslation();
  const [state, setState] = useState({
    text: null as string | null,
    remaining: 0,
    pending: false,
    error: false,
  });
  const [password, setPassword] = useState("");
  const [verifying, setVerifying] = useState(false);
  const controller = useRef<ReturnType<typeof createProtectedReveal> | null>(
    null,
  );
  const passwordRef = useRef("");
  const needsPassword = protection === "authenticate" && !isMacOverlay();
  useEffect(() => {
    const lease = captureChatOwnership();
    const conversation = useChat.getState().active;
    if (!lease.owner || !conversation || !id) return;
    let mounted = true;
    const current = () =>
      mounted &&
      lease.current() &&
      !useAppLock.getState().locked &&
      useChat.getState().active?.id === conversation.id &&
      useChat.getState().active?.kind === conversation.kind;
    const instance = createProtectedReveal({
      nativeAuthentication: protection === "authenticate" && isMacOverlay(),
      current,
      foreground: () =>
        document.visibilityState === "visible" && document.hasFocus(),
      reveal: (signal) => {
        const secret = passwordRef.current;
        passwordRef.current = "";
        setPassword("");
        const reference = {
          kind: conversation.kind,
          target: conversation.id,
          message_id: id,
        };
        return image
          ? chat
              .revealImage(lease.owner!, reference, secret || undefined)
              .then(async (bytes) => ({
                text: await createProtectedImageUrl(
                  bytes,
                  mime || "image/png",
                  signal,
                ),
                visible_for_ms: 30000,
              }))
          : chat.revealMessage(lease.owner!, reference, secret || undefined);
      },
      disposeBody: image ? (url) => URL.revokeObjectURL(url) : undefined,
      changed: (next) => {
        if (mounted) setState(next);
      },
    });
    controller.current = instance;
    const check = () => {
      if (!current()) {
        instance.hide();
        setVerifying(false);
        setPassword("");
        passwordRef.current = "";
      }
    };
    const subscriptions = [
      useChat.subscribe(check),
      useAuth.subscribe(check),
      useAppLock.subscribe(check),
    ];
    const blur = () => {
      instance.blur();
      setVerifying(false);
      setPassword("");
      passwordRef.current = "";
    };
    const focus = () => instance.focus();
    const anotherReveal = () => instance.hide();
    window.addEventListener("protected-message-reveal", anotherReveal);
    const visibility = () => {
      if (document.visibilityState !== "visible") {
        instance.hide();
        setPassword("");
        passwordRef.current = "";
        setVerifying(false);
      }
    };
    window.addEventListener("blur", blur);
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", visibility);
    return () => {
      mounted = false;
      instance.hide();
      controller.current = null;
      passwordRef.current = "";
      subscriptions.forEach((unsubscribe) => unsubscribe());
      window.removeEventListener("protected-message-reveal", anotherReveal);
      window.removeEventListener("blur", blur);
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", visibility);
    };
  }, [id, protection, image, mime]);
  const reveal = () => {
    window.dispatchEvent(new Event("protected-message-reveal"));
    setVerifying(false);
    void controller.current?.reveal();
  };
  return (
    <div
      data-testid="protected-message"
      className="min-w-0 max-w-full space-y-2"
      onCopy={(event) => event.preventDefault()}
      onCut={(event) => event.preventDefault()}
    >
      <div className="flex items-center gap-2 text-xs">
        <LockKeyhole aria-hidden="true" className="h-3.5 w-3.5" />
        {t("protectedMessage.label")}
      </div>
      {state.text !== null ? (
        <>
          <div
            data-testid="protected-message-body"
            className="select-none whitespace-pre-wrap [overflow-wrap:anywhere]"
          >
            {image ? (
              <img
                src={state.text}
                alt={t("protectedMessage.image")}
                draggable={false}
                className="max-h-80 max-w-full object-contain"
              />
            ) : (
              state.text
            )}
          </div>
          <div className="flex items-center justify-between gap-4 text-xs">
            <span className="tabular-nums">
              {t("protectedMessage.remaining", { count: state.remaining })}
            </span>
            <button
              type="button"
              onClick={() => controller.current?.hide()}
              className="rounded underline underline-offset-2 focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("protectedMessage.hide")}
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="text-xs">{t("protectedMessage.duration")}</p>
          {verifying ? (
            <form
              onSubmit={(event) => {
                event.preventDefault();
                reveal();
              }}
              className="flex flex-wrap gap-2"
            >
              <input
                autoFocus
                type="password"
                autoComplete="current-password"
                aria-label={t("login.password")}
                value={password}
                onChange={(event) => {
                  setPassword(event.target.value);
                  passwordRef.current = event.target.value;
                }}
                className="min-w-0 rounded-md border border-input bg-background px-2 py-1 text-sm"
              />
              <button
                type="submit"
                disabled={!password}
                className="rounded px-2 py-1 underline disabled:opacity-50"
              >
                {t("protectedMessage.verify")}
              </button>
              <button
                type="button"
                onClick={() => {
                  setVerifying(false);
                  setPassword("");
                  passwordRef.current = "";
                  controller.current?.hide();
                }}
                className="rounded px-2 py-1"
              >
                {t("common.cancel")}
              </button>
            </form>
          ) : (
            <button
              type="button"
              disabled={!id || state.pending}
              onClick={() => (needsPassword ? setVerifying(true) : reveal())}
              className="rounded-md border border-border px-3 py-1.5 text-sm hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
            >
              {state.pending
                ? t("protectedMessage.opening")
                : t(
                    protection === "authenticate"
                      ? "protectedMessage.verify"
                      : "protectedMessage.open",
                  )}
            </button>
          )}
          {state.error && (
            <p role="alert" className="text-xs text-destructive">
              {t("protectedMessage.failed")}
            </p>
          )}
        </>
      )}
    </div>
  );
}
