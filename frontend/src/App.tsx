import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Loader2, LockKeyhole } from "lucide-react";
import { useTranslation } from "react-i18next";
import { LoginScreen } from "@/features/auth/LoginScreen";
import { WindowControls } from "@/components/WindowControls";
import { isMacOverlay } from "@/lib/platform";
import { useAuth } from "@/store/auth";
import { useAvatars } from "@/store/avatars";
import { useAppLock } from "@/store/appLock";
import { useSettings } from "@/store/settings";
import { useCalls } from "@/store/calls";
import { auth } from "@/lib/api";
import { listen } from "@tauri-apps/api/event";

const ChatApp = lazy(() =>
  import("@/features/chat/ChatApp").then(({ ChatApp }) => ({
    default: ChatApp,
  })),
);

export default function App() {
  const { t } = useTranslation();
  const user = useAuth((s) => s.user);
  const owner = user?.id ?? null;
  const booting = useAuth((s) => s.booting);
  const tryAutoLogin = useAuth((s) => s.tryAutoLogin);
  const loadAvatars = useAvatars((s) => s.load);
  const locked = useAppLock((s) => s.locked);
  const lock = useAppLock((s) => s.lock);
  const refreshLock = useAppLock((s) => s.refresh);
  const autoLockSeconds = useSettings((s) => s.privacy.auto_lock_seconds);
  const lockWhenHidden = useSettings((s) => s.privacy.lock_when_hidden);
  const callPhase = useCalls((s) => s.phase);

  // On app start, attempt a "stay signed in" resume from the OS keychain BEFORE deciding
  // whether to show the login screen. While it's in flight we show a brief unlocking
  // splash so a remembered session never flashes the login form.
  useEffect(() => {
    void (async () => {
      try {
        const lockedUser = await auth.lockStatus();
        if (lockedUser) {
          useAppLock.setState({ locked: true });
          useAuth.setState({ user: lockedUser, booting: false });
          return;
        }
      } catch {
        /* browser preview or native bridge unavailable */
      }
      await tryAutoLogin();
    })();
  }, [tryAutoLogin]);

  useEffect(() => {
    useAvatars.getState().setOwner(owner);
    useSettings.getState().setOwner(owner);
    if (owner && !locked) void loadAvatars();
  }, [owner, locked, loadAvatars]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void listen("app-locked", () => {
      void useAppLock.getState().refresh();
    })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!user || locked) return;
    let lastSent = 0;
    const activity = () => {
      const now = Date.now();
      if (now - lastSent < 1000) return;
      lastSent = now;
      void auth.noteActivity().catch(() => {});
    };
    activity();
    window.addEventListener("pointerdown", activity);
    window.addEventListener("keydown", activity);
    return () => {
      window.removeEventListener("pointerdown", activity);
      window.removeEventListener("keydown", activity);
    };
  }, [user, locked]);

  useEffect(() => {
    if (user && !locked)
      void auth
        .setCallActive(callPhase !== "idle" && callPhase !== "ended")
        .catch(() => {});
  }, [user, locked, callPhase]);

  useEffect(() => {
    if (!user || locked) return;
    const check = () => {
      void refreshLock();
    };
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    return () => {
      window.removeEventListener("focus", check);
      document.removeEventListener("visibilitychange", check);
    };
  }, [user, locked, refreshLock]);

  useEffect(() => {
    if (!user || locked || autoLockSeconds === 0) return;
    let timer: ReturnType<typeof setTimeout>;
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        if (useCalls.getState().phase !== "idle") {
          schedule();
        } else {
          void lock();
        }
      }, autoLockSeconds * 1000);
    };
    const onVisibility = () => {
      if (
        document.hidden &&
        lockWhenHidden &&
        useCalls.getState().phase === "idle"
      ) {
        void lock();
      } else if (document.hidden) {
        schedule();
      } else {
        void refreshLock();
        schedule();
      }
    };
    const onFocus = () => {
      void refreshLock();
      schedule();
    };
    schedule();
    window.addEventListener("pointerdown", schedule);
    window.addEventListener("keydown", schedule);
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("pointerdown", schedule);
      window.removeEventListener("keydown", schedule);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [user, locked, autoLockSeconds, lockWhenHidden, lock, refreshLock]);

  return (
    <>
      {/* macOS overlay title bar is transparent and covered by the webview, so the top
          title-bar band isn't draggable unless we mark it. One full-width strip (the
          ~28px traffic-light zone, where no controls sit — the headers are inset below it)
          makes every screen draggable. Frameless (Win/Linux) drags via WindowControls +
          the per-screen strips. */}
      {isMacOverlay() && (
        <div
          aria-hidden
          data-tauri-drag-region
          className="fixed inset-x-0 top-0 z-40 h-7"
        />
      )}
      {/* Custom min/max/close for the frameless window (Windows/Linux); null on macOS/web. */}
      <WindowControls />
      {user && locked ? (
        <LockScreen />
      ) : user ? (
        <Suspense
          fallback={<Unlocking label={t("redesign.loadingContacts")} />}
        >
          <ChatApp />
        </Suspense>
      ) : booting ? (
        <Unlocking label={t("login.resuming")} />
      ) : (
        <LoginScreen />
      )}
    </>
  );
}

function LockScreen() {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const attemptedMacAuth = useRef(false);
  const busy = useAppLock((s) => s.busy);
  const error = useAppLock((s) => s.error);
  const unlock = useAppLock((s) => s.unlock);
  const logout = useAuth((s) => s.logout);
  useEffect(() => {
    if (!isMacOverlay() || busy || attemptedMacAuth.current) return;
    const tryMacAuth = () => {
      if (document.hidden || attemptedMacAuth.current) return;
      attemptedMacAuth.current = true;
      void unlock();
    };
    tryMacAuth();
    document.addEventListener("visibilitychange", tryMacAuth);
    return () => document.removeEventListener("visibilitychange", tryMacAuth);
  }, [unlock, busy]);
  return (
    <main
      data-testid="lock-screen"
      className="flex h-full items-center justify-center bg-background px-6"
    >
      <form
        className="w-full max-w-sm space-y-4 rounded-xl border border-border bg-card p-7 shadow-elevation"
        onSubmit={(event) => {
          event.preventDefault();
          if (!isMacOverlay()) void unlock(password);
        }}
      >
        <LockKeyhole
          className="h-7 w-7 text-muted-foreground"
          aria-hidden="true"
        />
        <h1 className="text-xl font-semibold">{t("settings.lockedTitle")}</h1>
        <p className="text-sm text-muted-foreground">
          {t("settings.lockedDescription")}
        </p>
        {!isMacOverlay() && (
          <input
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            placeholder={t("login.password")}
            className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
          />
        )}
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        {!isMacOverlay() && (
          <button
            type="submit"
            disabled={busy || !password}
            className="w-full rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50"
          >
            {t("settings.unlock")}
          </button>
        )}
        {isMacOverlay() && (
          <button
            type="button"
            disabled={busy}
            onClick={() => void unlock()}
            className="w-full rounded-md border border-border px-4 py-2 text-sm disabled:opacity-50"
          >
            {t("settings.useTouchId")}
          </button>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            void logout().finally(() =>
              useAppLock.setState({ locked: false, error: null }),
            );
          }}
          className="w-full px-4 py-2 text-sm text-muted-foreground hover:text-foreground"
        >
          {t("sidebar.signOut")}
        </button>
      </form>
    </main>
  );
}

/** A minimal centered splash shown while the auto-login attempt resolves. */
function Unlocking({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center bg-background">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        {label}
      </div>
    </div>
  );
}
