import { useEffect, useState } from "react";
import { Loader2, ShieldCheck } from "lucide-react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Logo } from "@/components/Logo";
import { fadeSlideUp, useMotionOK } from "@/lib/motion";
import { settings as settingsApi } from "@/lib/api";
import { needsCustomWindowControls } from "@/lib/platform";
import { useAuth } from "@/store/auth";

/**
 * The hero. Opening on the most characteristic thing in mesh-talk's world: a
 * cryptographic identity on a calm ink surface. The locally bundled brand mark and
 * clear sign-in form keep first run focused on the account the user is unlocking.
 */
export function LoginScreen() {
  const { t } = useTranslation();
  const ok = useMotionOK();
  const { login, register, loading, error, clearError } = useAuth();
  const [tab, setTab] = useState<"signin" | "register">("signin");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  // "Stay signed in" reflects the persisted backend setting (default on); the login
  // command reads it server-side, so we persist the toggle as the user flips it.
  const [stay, setStay] = useState(true);

  useEffect(() => {
    void settingsApi.get().then(
      (s) => setStay(s.stay_signed_in),
      () => {},
    );
  }, []);

  const onStay = (v: boolean) => {
    setStay(v);
    void settingsApi.update({ stay_signed_in: v }).catch(() => {});
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setNotice(null);
    if (tab === "signin") {
      await login(username.trim(), password);
    } else {
      const okReg = await register(username.trim(), password);
      if (okReg) {
        setNotice(t("login.accountCreated"));
        setTab("signin");
        setPassword("");
      }
    }
  };

  const onTab = (v: string) => {
    setTab(v as "signin" | "register");
    clearError();
    setNotice(null);
  };

  return (
    <div className="relative flex h-full items-center justify-center overflow-hidden bg-background p-6">
      {/* Frameless window (Windows/Linux) has no native bar to grab — a thin top strip drags
          it. (macOS is covered by the app-level top drag strip in App.tsx.) Skipped in a
          plain browser (e2e/dev tab) where there's no window to move. */}
      {needsCustomWindowControls() && (
        <div
          data-tauri-drag-region
          className="absolute inset-x-0 top-0 z-10 h-8"
        />
      )}
      <div className="relative w-full max-w-sm">
        <motion.div
          initial={false}
          animate="visible"
          variants={fadeSlideUp}
          className="mb-8 flex flex-col items-center text-center"
        >
          {/* The app mark opens the first-run screen; IdentityGlyph remains the avatar. */}
          <div className="relative mb-5">
            <motion.div
              initial={false}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ duration: 0.32, ease: [0.22, 1, 0.36, 1] }}
            >
              <Logo size={80} title="Mesh-Talk" />
            </motion.div>
          </div>

          <h1 className="font-display text-3xl font-semibold tracking-tight">
            Mesh-Talk
          </h1>
          <p className="mt-1.5 flex items-center gap-1.5 text-sm text-muted-foreground">
            <ShieldCheck className="h-3.5 w-3.5 text-verified" />
            {t("login.tagline")}
          </p>
        </motion.div>

        <motion.div
          initial={false}
          animate="visible"
          variants={fadeSlideUp}
          transition={{ delay: ok ? 0.06 : 0 }}
          className="rounded-xl border bg-card p-6 shadow-elevation"
        >
          <Tabs value={tab} onValueChange={onTab}>
            <TabsList className="grid w-full grid-cols-2">
              <TabsTrigger value="signin" data-testid="login-tab-signin">
                {t("login.signIn")}
              </TabsTrigger>
              <TabsTrigger value="register" data-testid="login-tab-register">
                {t("login.register")}
              </TabsTrigger>
            </TabsList>

            <TabsContent value={tab} forceMount>
              <form
                onSubmit={submit}
                className="mt-5 space-y-4"
                data-testid="login-form"
              >
                <div className="space-y-2">
                  <Label htmlFor="username">{t("login.username")}</Label>
                  <Input
                    id="username"
                    data-testid="login-username"
                    autoFocus
                    autoComplete="username"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    placeholder={t("login.usernamePlaceholder")}
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="password">{t("login.password")}</Label>
                  <Input
                    id="password"
                    data-testid="login-password"
                    type="password"
                    autoComplete={
                      tab === "signin" ? "current-password" : "new-password"
                    }
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    placeholder={
                      tab === "register"
                        ? t("login.passwordHint")
                        : t("login.passwordPlaceholder")
                    }
                  />
                </div>

                {tab === "signin" && (
                  <label className="flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
                    <input
                      type="checkbox"
                      data-testid="login-stay-signed-in"
                      checked={stay}
                      onChange={(e) => onStay(e.target.checked)}
                      className="h-4 w-4 rounded border-input accent-signal"
                    />
                    {t("login.staySignedIn")}
                  </label>
                )}

                {error && (
                  <p className="text-sm font-medium text-destructive">
                    {error}
                  </p>
                )}
                {notice && (
                  <p className="text-sm font-medium text-signal">{notice}</p>
                )}

                <Button
                  type="submit"
                  data-testid="login-submit"
                  className="w-full"
                  disabled={loading || !username.trim() || !password}
                >
                  {loading && <Loader2 className="h-4 w-4 animate-spin" />}
                  {tab === "signin"
                    ? t("login.signIn")
                    : t("login.createAccount")}
                </Button>
              </form>
            </TabsContent>
          </Tabs>
        </motion.div>

        <p className="mt-6 text-center text-xs text-muted-foreground">
          {t("login.footer")}
        </p>
      </div>
    </div>
  );
}
