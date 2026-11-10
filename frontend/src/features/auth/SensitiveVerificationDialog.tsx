import { useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  registerSensitiveVerification,
  type SensitiveCredential,
} from "@/lib/api/sensitiveInvoke";
import { useAuth } from "@/store/auth";
import { useAppLock } from "@/store/appLock";

const actionKey = (command: string) => {
  switch (command) {
    case "create_backup":
    case "google_drive_create_backup":
      return "securityVerification.backup";
    case "start_linking":
    case "link_device":
      return "securityVerification.linking";
    case "set_local_agent_access":
      return "securityVerification.agent";
    case "retry_saved_session":
      return "securityVerification.savedLogin";
    default:
      return "securityVerification.settings";
  }
};

/** One password for one native operation. Native macOS prompts do not use this view. */
export function SensitiveVerificationDialog({
  owner,
  generation,
}: {
  owner: string;
  generation: number;
}) {
  const { t } = useTranslation();
  const [command, setCommand] = useState<string | null>(null);
  const [password, setPassword] = useState("");
  const pending = useRef<
    ((credential: SensitiveCredential | null) => void) | null
  >(null);
  const current = () => {
    const auth = useAuth.getState();
    return (
      auth.user?.id === owner &&
      auth.generation === generation &&
      !useAppLock.getState().locked
    );
  };
  const finish = (credential: SensitiveCredential | null) => {
    const resolve = pending.current;
    pending.current = null;
    setPassword("");
    setCommand(null);
    resolve?.(credential);
  };
  useLayoutEffect(() => {
    const unregister = registerSensitiveVerification((action) => {
      if (!current() || pending.current) return Promise.resolve(null);
      return new Promise((resolve) => {
        pending.current = resolve;
        setPassword("");
        setCommand(action);
      });
    });
    return () => {
      unregister();
      pending.current?.(null);
      pending.current = null;
    };
    // Owner and generation are the complete lifetime of this mounted prompt host.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [owner, generation]);
  return (
    <Dialog
      open={command !== null}
      onOpenChange={(open) => {
        if (!open) finish(null);
      }}
    >
      <DialogContent data-testid="sensitive-verification-dialog">
        <DialogHeader>
          <DialogTitle>{t("securityVerification.title")}</DialogTitle>
          <DialogDescription>
            {t("securityVerification.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="text-sm font-medium">
          {command && t(actionKey(command))}
        </p>
        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (!current()) {
              finish(null);
              return;
            }
            if (!password) return;
            finish({ password, current });
          }}
        >
          <div className="space-y-2">
            <label
              htmlFor="sensitive-operation-password"
              className="text-sm font-medium"
            >
              {t("login.password")}
            </label>
            <Input
              id="sensitive-operation-password"
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
            />
          </div>
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              onClick={() => finish(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={!password}>
              {t("securityVerification.continue")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
