import { useEffect, useState } from "react";
import { Check, Pencil, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { AvatarEditMenu } from "@/components/identity/AvatarEditMenu";
import { IdentityGlyph } from "@/components/identity/IdentityGlyph";
import { shortId } from "@/lib/format";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";

/**
 * ProfileDialog — the user's own "profile card", opened by clicking their avatar/name in
 * the sidebar (the WeChat/Telegram pattern: tap yourself → a profile where you change your
 * photo and name). Avatar editing reuses {@link AvatarEditMenu}; the name is edited inline
 * via `useAuth.rename`. The login username and account id are shown read-only — they're the
 * stable identity, only the display name (nickname) and photo are editable.
 */
export function ProfileDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const user = useAuth((s) => s.user);
  const rename = useAuth((s) => s.rename);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);

  const username = user?.username ?? "";
  const displayName = user?.display_name || username;
  // Same seed the sidebar uses, so the photo/glyph matches everywhere.
  const seed = myAccountId || myId || username;

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(displayName);
  const [busy, setBusy] = useState(false);

  // Seed the draft when (re)opening, and drop back to read-only when the stored name changes.
  useEffect(() => {
    if (open) {
      setDraft(displayName);
      setEditing(false);
    }
  }, [open, displayName]);

  const save = async () => {
    const next = draft.trim();
    if (!next || next === displayName) {
      setEditing(false);
      return;
    }
    setBusy(true);
    const ok = await rename(next);
    setBusy(false);
    if (ok) setEditing(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm" data-testid="profile-dialog">
        <DialogHeader>
          <DialogTitle>{t("profile.title")}</DialogTitle>
          <DialogDescription>{t("profile.description")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col items-center gap-4 py-1">
          {/* Editable avatar — click to set a photo / pick from the gallery / remove. */}
          <AvatarEditMenu
            id={seed}
            ariaLabel={t("avatar.editOwn")}
            category="personal"
          >
            <IdentityGlyph seed={seed} size={96} title={displayName} />
          </AvatarEditMenu>

          {/* Editable display name (nickname). */}
          {editing ? (
            <div className="flex w-full max-w-[18rem] items-center gap-1.5">
              <Input
                autoFocus
                data-testid="profile-name-input"
                value={draft}
                maxLength={50}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void save();
                  if (e.key === "Escape") setEditing(false);
                }}
                className="h-9 text-center"
                aria-label={t("profile.displayName")}
              />
              <Button
                size="icon"
                variant="ghost"
                disabled={busy}
                onClick={() => void save()}
                data-testid="profile-name-save"
                aria-label={t("common.save")}
              >
                <Check className="h-4 w-4" />
              </Button>
              <Button
                size="icon"
                variant="ghost"
                onClick={() => setEditing(false)}
                aria-label={t("common.cancel")}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          ) : (
            <button
              type="button"
              data-testid="profile-name"
              onClick={() => setEditing(true)}
              className="group inline-flex items-center gap-1.5 rounded-md px-2 py-1 hover:bg-accent"
              title={t("profile.editName")}
            >
              <span
                data-testid="profile-name-text"
                className="font-display text-lg font-semibold tracking-tight"
              >
                {displayName}
              </span>
              <Pencil className="h-3.5 w-3.5 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
            </button>
          )}

          {/* Read-only identity — the stable handle + account fingerprint. The username is
              the LOGIN credential (the nickname above is not), so it's labelled and hinted
              explicitly: users must not think they sign in with their display name. */}
          <dl className="w-full space-y-2 rounded-lg border p-3 text-sm">
            <div className="flex items-center justify-between gap-3">
              <dt className="text-muted-foreground">{t("profile.username")}</dt>
              <dd
                data-testid="profile-username"
                className="truncate font-mono text-xs font-semibold"
                title={username}
              >
                {username}
              </dd>
            </div>
            {myAccountId && (
              <div className="flex items-center justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("profile.accountId")}
                </dt>
                <dd className="truncate font-mono text-xs" title={myAccountId}>
                  {shortId(myAccountId, 12)}
                </dd>
              </div>
            )}
            <p className="border-t pt-2 text-xs leading-relaxed text-muted-foreground">
              {t("profile.usernameHint")}
            </p>
          </dl>
        </div>
      </DialogContent>
    </Dialog>
  );
}
