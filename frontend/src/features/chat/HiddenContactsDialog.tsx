import { useMemo, useState } from "react";
import { EyeOff, Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { IdentityGlyph } from "@/components/identity";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useContactPolicy } from "@/store/contactPolicy";
import type { HiddenContact } from "@/lib/types";
import { shortId } from "@/lib/format";

export function HideContactDialog({
  contact,
  onClose,
}: {
  contact: HiddenContact | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const owner = useAuth((s) => s.user?.id);
  const busy = useContactPolicy((s) => s.busy);
  const error = useContactPolicy((s) => s.error);
  const loaded = useContactPolicy((s) => s.loaded);
  const policyOwner = useContactPolicy((s) => s.owner);
  const save = async () => {
    if (!contact || !owner) return;
    const ok = await useContactPolicy
      .getState()
      .change(owner, contact.account_id, true, contact.name);
    if (!ok || useAuth.getState().user?.id !== owner) return;
    const active = useChat.getState().active;
    if (active?.kind === "account" && active.id === contact.account_id)
      useChat.setState({ active: null });
    onClose();
  };
  return (
    <Dialog
      open={contact !== null}
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent
        data-testid="hide-contact-dialog"
        className="max-w-md"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          document.getElementById("hide-contact-cancel")?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{t("contactVisibility.confirmTitle")}</DialogTitle>
          <DialogDescription>
            {t("contactVisibility.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="text-xs text-muted-foreground">
          {t("contactVisibility.localOnly")}
        </p>
        <div className="min-w-0 rounded-lg border p-3">
          <p
            className="line-clamp-2 break-all text-sm font-medium"
            title={contact?.name}
          >
            {contact?.name}
          </p>
          <p className="break-all font-mono text-xs text-muted-foreground">
            {contact?.account_id}
          </p>
        </div>
        {error === "save" && (
          <p role="alert" className="text-sm text-destructive">
            {t("contactVisibility.saveError")}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button
            id="hide-contact-cancel"
            data-testid="hide-contact-cancel"
            variant="outline"
            disabled={busy}
            onClick={onClose}
          >
            {t("common.cancel")}
          </Button>
          <Button
            data-testid="hide-contact-confirm"
            disabled={busy || !loaded || owner !== policyOwner}
            onClick={() => void save()}
          >
            {busy ? t("common.loading") : t("contactVisibility.hide")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** Adds a shortcut without changing the narrow sidebar's row layout. Settings is the discoverable keyboard path. */
export function ContactContextMenu({
  account,
  name,
  children,
}: {
  account: string;
  name: string;
  children: React.ReactNode;
}) {
  const { t } = useTranslation();
  const [confirm, setConfirm] = useState(false);
  const loaded = useContactPolicy((s) => s.loaded);
  const myAccountId = useChat((s) => s.myAccountId);
  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger asChild data-context-menu>
          {children}
        </ContextMenuTrigger>
        <ContextMenuContent>
          <ContextMenuItem
            data-testid={`hide-contact-menu-${account}`}
            disabled={!loaded || account === myAccountId}
            onSelect={() => {
              useContactPolicy.getState().clearSaveError();
              setConfirm(true);
            }}
          >
            <EyeOff className="mr-2 h-4 w-4" />
            {t("contactVisibility.hide")}
          </ContextMenuItem>
        </ContextMenuContent>
      </ContextMenu>
      <HideContactDialog
        contact={confirm ? { account_id: account, name } : null}
        onClose={() => setConfirm(false)}
      />
    </>
  );
}

export function HiddenContactsDialog() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [confirm, setConfirm] = useState<HiddenContact | null>(null);
  const [tab, setTab] = useState("hidden");
  const contacts = useContactPolicy((s) => s.contacts);
  const loaded = useContactPolicy((s) => s.loaded);
  const loading = useContactPolicy((s) => s.loading);
  const busy = useContactPolicy((s) => s.busy);
  const error = useContactPolicy((s) => s.error);
  const owner = useAuth((s) => s.user?.id);
  const accounts = useChat((s) => s.accounts);
  const favorites = useChat((s) => s.favorites);
  const myAccountId = useChat((s) => s.myAccountId);
  const ready = useChat((s) => s.ready);
  const hidden = useMemo(() => Object.values(contacts), [contacts]);
  const candidates = useMemo(
    () =>
      accounts
        .filter((a) => a.account_id !== myAccountId && !contacts[a.account_id])
        .map((a) => ({
          account_id: a.account_id,
          name:
            favorites[a.account_id]?.custom_alias ||
            a.names[0] ||
            shortId(a.account_id),
        })),
    [accounts, myAccountId, contacts, favorites],
  );
  const matches = (c: HiddenContact) =>
    `${c.name} ${c.account_id}`
      .toLocaleLowerCase()
      .includes(query.trim().toLocaleLowerCase());
  const renderList = (list: HiddenContact[], restore: boolean) => {
    const filtered = list.filter(matches);
    return (
      <div
        className="max-h-[35vh] space-y-1 overflow-y-auto"
        role="list"
        aria-label={t(
          restore ? "contactVisibility.hidden" : "contactVisibility.add",
        )}
      >
        {filtered.length === 0 && (
          <p className="px-2 py-6 text-center text-sm text-muted-foreground">
            {t(
              query.trim()
                ? "contactVisibility.noMatches"
                : restore
                  ? "contactVisibility.emptyHidden"
                  : "contactVisibility.noCandidates",
            )}
          </p>
        )}
        {filtered.map((c) => (
          <div
            role="listitem"
            key={c.account_id}
            className="flex items-center gap-3 rounded-lg border p-3"
          >
            <IdentityGlyph seed={c.account_id} size={32} title={c.name} />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium" title={c.name}>
                {c.name}
              </p>
              <p
                className="truncate font-mono text-xs text-muted-foreground"
                title={c.account_id}
              >
                {shortId(c.account_id, 12)}
              </p>
            </div>
            <Button
              variant="outline"
              size="sm"
              data-testid={`${restore ? "restore" : "hide"}-contact-${c.account_id}`}
              aria-label={`${t(restore ? "contactVisibility.restore" : "contactVisibility.hide")} ${c.name}`}
              disabled={busy || loading || (!restore && !ready)}
              onClick={() => {
                if (restore && owner)
                  void useContactPolicy
                    .getState()
                    .change(owner, c.account_id, false, c.name);
                else {
                  useContactPolicy.getState().clearSaveError();
                  setConfirm(c);
                }
              }}
            >
              {t(
                restore
                  ? "contactVisibility.restore"
                  : "contactVisibility.hide",
              )}
            </Button>
          </div>
        ))}
      </div>
    );
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        setOpen(value);
        if (!value) {
          setQuery("");
          setTab("hidden");
        }
      }}
    >
      <DialogTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          data-testid="manage-hidden-contacts"
        >
          {t("contactVisibility.manage")}
          {loaded && hidden.length > 0 ? ` (${hidden.length})` : ""}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-lg" data-testid="hidden-contacts-dialog">
        <DialogHeader>
          <DialogTitle>{t("contactVisibility.title")}</DialogTitle>
          <DialogDescription>
            {t("contactVisibility.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="text-xs text-muted-foreground">
          {t("contactVisibility.localOnly")}
        </p>
        {loading && (
          <p role="status" className="text-sm text-muted-foreground">
            {t("common.loading")}
          </p>
        )}
        {error && (
          <div role="alert" className="space-y-2 text-sm text-destructive">
            <p>
              {t(
                error === "save"
                  ? "contactVisibility.saveError"
                  : "contactVisibility.loadError",
              )}
            </p>
            {error === "load" && (
              <Button
                variant="outline"
                size="sm"
                disabled={loading || busy}
                onClick={() => {
                  if (owner) void useContactPolicy.getState().load(owner);
                }}
              >
                {t("contactVisibility.retry")}
              </Button>
            )}
          </div>
        )}
        {loaded && (
          <Tabs
            value={tab}
            onValueChange={(value) => {
              setTab(value);
              setQuery("");
            }}
          >
            <TabsList className="w-full">
              <TabsTrigger
                className="flex-1"
                value="hidden"
                data-testid="hidden-contacts-hidden-tab"
              >
                {t("contactVisibility.hidden")} ({hidden.length})
              </TabsTrigger>
              <TabsTrigger
                className="flex-1"
                value="add"
                data-testid="hidden-contacts-add-tab"
              >
                {t("contactVisibility.add")}
              </TabsTrigger>
            </TabsList>
            <div className="relative mt-3">
              <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                data-testid="hidden-contacts-search"
                className="pl-9"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("contactVisibility.search")}
                aria-label={t("contactVisibility.search")}
              />
            </div>
            <TabsContent value="hidden">{renderList(hidden, true)}</TabsContent>
            <TabsContent value="add">
              {renderList(candidates, false)}
            </TabsContent>
          </Tabs>
        )}
        <HideContactDialog contact={confirm} onClose={() => setConfirm(null)} />
      </DialogContent>
    </Dialog>
  );
}
