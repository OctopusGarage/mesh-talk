import { useEffect } from "react";
import { Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { LoginScreen } from "@/features/auth/LoginScreen";
import { ChatApp } from "@/features/chat/ChatApp";
import { WindowControls } from "@/components/WindowControls";
import { isMacOverlay } from "@/lib/platform";
import { useAuth } from "@/store/auth";
import { useAvatars } from "@/store/avatars";

export default function App() {
  const { t } = useTranslation();
  const user = useAuth((s) => s.user);
  const booting = useAuth((s) => s.booting);
  const tryAutoLogin = useAuth((s) => s.tryAutoLogin);
  const loadAvatars = useAvatars((s) => s.load);

  // On app start, attempt a "stay signed in" resume from the OS keychain BEFORE deciding
  // whether to show the login screen. While it's in flight we show a brief unlocking
  // splash so a remembered session never flashes the login form. Also load the locally
  // stored custom avatars (no node needed — pure local UI personalization).
  useEffect(() => {
    void tryAutoLogin();
    void loadAvatars();
  }, [tryAutoLogin, loadAvatars]);

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
      {user ? (
        <ChatApp />
      ) : booting ? (
        <Unlocking label={t("login.resuming")} />
      ) : (
        <LoginScreen />
      )}
    </>
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
