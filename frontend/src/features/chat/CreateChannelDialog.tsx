import { useMemo, useState } from "react";
import { motion } from "framer-motion";
import { Check, Plus, Loader2, Users } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { IdentityCrest } from "@/components/identity";
import { cn } from "@/lib/utils";
import { errorMessage } from "@/lib/error";
import {
  fadeSlideUp,
  listStagger,
  MAX_STAGGER_ITEMS,
  useMotionOK,
} from "@/lib/motion";
import { useChat } from "@/store/chat";
import { useContactPolicy } from "@/store/contactPolicy";
import { visiblePeers } from "@/lib/contactVisibility";
import { channelInviteAccounts } from "./channelMemberProjection";
import { peerAccountKey } from "@/lib/rosterProjection";
import { displayIdentity } from "@/lib/displayIdentity";
import { VirtualRosterList } from "./VirtualRosterList";

export function CreateChannelDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const rawPeers = useChat((s) => s.peers);
  const myAccountId = useChat((s) => s.myAccountId);
  const accounts = useChat((s) => s.accounts);
  const favorites = useChat((s) => s.favorites);
  const contacts = useContactPolicy((s) => s.contacts);
  const loaded = useContactPolicy((s) => s.loaded);
  const peers = useMemo(
    () =>
      loaded
        ? channelInviteAccounts(
            visiblePeers(rawPeers, contacts).filter(
              (peer) => peer.account_id && peer.account_id !== myAccountId,
            ),
          )
        : [],
    [rawPeers, contacts, loaded, myAccountId],
  );
  const animatePeers = motionOK && peers.length <= MAX_STAGGER_ITEMS;
  const createChannel = useChat((s) => s.createChannel);

  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const toggle = (id: string) => setSelected((s) => ({ ...s, [id]: !s[id] }));
  const selectedCount = peers.filter((p) => selected[peerAccountKey(p)]).length;

  const submit = async () => {
    const ids = peers
      .filter((p) => selected[peerAccountKey(p)])
      .map(peerAccountKey);
    if (!name.trim() || ids.length === 0) return;
    setCreateError(null);
    setBusy(true);
    try {
      const created = await createChannel(name.trim(), ids);
      if (!created) return;
      setOpen(false);
      setName("");
      setSelected({});
    } catch (error) {
      setCreateError(t("createChannel.failed", { error: errorMessage(error) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (busy) return;
        setOpen(next);
        if (!next) setCreateError(null);
      }}
    >
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          title={t("createChannel.trigger")}
          aria-label={t("createChannel.trigger")}
        >
          <Plus className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent data-testid="create-channel-dialog">
        <DialogHeader>
          <DialogTitle>{t("createChannel.title")}</DialogTitle>
          <DialogDescription>
            {t("createChannel.description")}
          </DialogDescription>
        </DialogHeader>

        <Input
          autoFocus
          placeholder={t("createChannel.namePlaceholder")}
          value={name}
          onChange={(e) => setName(e.target.value)}
          aria-label={t("createChannel.namePlaceholder")}
          disabled={busy}
        />

        <div className="space-y-1.5">
          <div className="flex items-center gap-2 text-[12px] font-semibold text-muted-foreground">
            <Users className="h-3.5 w-3.5" />
            {t("createChannel.invite")}
            {selectedCount > 0 && (
              <span className="text-signal">· {selectedCount}</span>
            )}
          </div>
          <motion.div
            initial={animatePeers ? "hidden" : false}
            animate="visible"
            variants={listStagger}
            className={peers.length === 0 ? "rounded-lg border p-1" : undefined}
          >
            {peers.length === 0 && (
              <p className="px-2 py-3 text-center text-sm text-muted-foreground">
                {t("createChannel.noPeers")}
              </p>
            )}
            {peers.length > 0 && (
              <VirtualRosterList
                items={peers}
                maxHeight={224}
                className="space-y-0.5 overflow-y-auto rounded-lg border p-1"
                itemKey={peerAccountKey}
                renderItem={(p) => {
                  const key = peerAccountKey(p);
                  const on = !!selected[key];
                  const identity = displayIdentity({
                    accountId: p.account_id,
                    deviceId: p.user_id,
                    accounts,
                    peers: rawPeers,
                    favorites,
                  });
                  return (
                    <motion.button
                      key={key}
                      variants={fadeSlideUp}
                      onClick={() => toggle(key)}
                      aria-pressed={on}
                      disabled={busy}
                      className={cn(
                        "flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left transition-colors",
                        on ? "bg-signal/10" : "hover:bg-accent/50",
                      )}
                    >
                      <div className="min-w-0 flex-1">
                        <IdentityCrest
                          id={key}
                          avatarId={p.account_id ?? undefined}
                          name={identity.name}
                          variant="compact"
                        />
                      </div>
                      <span
                        className={cn(
                          "flex h-5 w-5 shrink-0 items-center justify-center rounded-md border transition-colors",
                          on
                            ? "border-signal bg-signal text-primary-foreground"
                            : "border-input",
                        )}
                      >
                        {on && <Check className="h-3.5 w-3.5" />}
                      </span>
                    </motion.button>
                  );
                }}
              />
            )}
          </motion.div>
        </div>

        {createError && (
          <p
            role="alert"
            data-testid="create-channel-error"
            className="break-words text-xs text-destructive"
          >
            {createError}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button
            variant="ghost"
            disabled={busy}
            onClick={() => setOpen(false)}
          >
            {t("common.cancel")}
          </Button>
          <Button
            disabled={!name.trim() || selectedCount === 0 || busy}
            onClick={submit}
          >
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            {t("common.create")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
