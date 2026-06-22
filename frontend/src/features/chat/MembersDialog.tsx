import { useState } from "react";
import { motion } from "framer-motion";
import { Users, UserPlus, X } from "lucide-react";
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
import { IdentityCrest } from "@/components/identity";
import { fadeSlideUp, listStagger, useMotionOK } from "@/lib/motion";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { OFFLINE, presenceStatus, usePresence } from "@/store/presence";

// We are always reachable to ourselves, and we never appear in our own discovery roster
// (so no peer/presence entry exists for us) — show the self member as online.
const SELF_ONLINE = presenceStatus({ online: true, last_seen_secs: 0 });

export function MembersDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const members = useChat((s) => s.members);
  const channelOwner = useChat((s) => s.channelOwner);
  const peers = useChat((s) => s.peers);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const myName = useAuth((s) => s.user?.username ?? "");
  const addMember = useChat((s) => s.addMember);
  const removeMember = useChat((s) => s.removeMember);
  // Whole presence map (the dialog reads many ids at once; one subscription).
  const presenceMap = usePresence((s) => s.map);
  const [open, setOpen] = useState(false);

  const statusFor = (accountId: string | null | undefined) =>
    presenceStatus((accountId ? presenceMap[accountId] : undefined) ?? OFFLINE);

  const memberIds = new Set(members.map((m) => m.user_id));
  const addable = peers.filter((p) => !memberIds.has(p.user_id));
  // Only the channel owner may change membership — the core enforces this (a non-owner's
  // add/remove is rejected by every node), so a non-owner only ever sees a read-only list.
  const isOwner = channelOwner !== "" && channelOwner === myId;

  // A member's account id (and so its presence) comes from the discovery roster.
  const accountOf = (userId: string) =>
    peers.find((p) => p.user_id === userId)?.account_id ?? null;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          title={t("members.trigger")}
          data-testid="members-trigger"
        >
          <Users className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-baseline gap-2">
            {t("members.title")}
            <span className="font-display text-base font-normal tabular-nums text-muted-foreground">
              {members.length}
            </span>
          </DialogTitle>
          <DialogDescription>{t("members.description")}</DialogDescription>
        </DialogHeader>

        <motion.div
          initial={motionOK ? "hidden" : false}
          animate="visible"
          variants={listStagger}
          className="max-h-56 space-y-0.5 overflow-y-auto"
        >
          {members.map((m) => {
            // Self isn't in the discovery roster, so it has no peer-derived name/presence:
            // show our own display name and treat ourselves as online.
            const isSelf = m.user_id === myId;
            const name = isSelf
              ? myName || m.name || t("common.unnamed")
              : m.name || t("common.unnamed");
            const status = isSelf
              ? SELF_ONLINE
              : statusFor(accountOf(m.user_id));
            // Key the crest by ACCOUNT id, not the device user_id: custom/propagated avatars
            // are stored per account (useAvatar reads account ids), so a device-keyed crest
            // would never resolve a member's photo. Fall back to the device id if the account
            // isn't known from the roster yet. (Also makes the glyph match the member's
            // account-keyed DM glyph.)
            const crestId = isSelf
              ? myAccountId || m.user_id
              : (accountOf(m.user_id) ?? m.user_id);
            const memberIsOwner = m.user_id === channelOwner;
            return (
              <motion.div
                key={m.user_id}
                variants={fadeSlideUp}
                className="group flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-accent/50"
              >
                <div className="min-w-0 flex-1">
                  <IdentityCrest
                    id={crestId}
                    name={name}
                    status={status}
                    variant="compact"
                  />
                </div>
                {memberIsOwner && (
                  <span
                    className="shrink-0 rounded-full bg-signal/15 px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wider text-signal"
                    data-testid="member-owner-badge"
                  >
                    {t("members.owner")}
                  </span>
                )}
                {/* Only the owner may remove members — the core rejects a non-owner's kick. */}
                {isOwner && !memberIsOwner && (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 shrink-0 text-muted-foreground opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100"
                    title={t("common.remove")}
                    aria-label={t("common.remove")}
                    onClick={() => removeMember(m.user_id)}
                  >
                    <X className="h-4 w-4" />
                  </Button>
                )}
              </motion.div>
            );
          })}
        </motion.div>

        {isOwner && addable.length > 0 && (
          <div className="space-y-1.5">
            <div className="font-mono text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
              {t("members.addPeer")}
            </div>
            <div className="max-h-40 space-y-0.5 overflow-y-auto rounded-lg border p-1">
              {addable.map((p) => (
                <button
                  key={p.user_id}
                  onClick={() => addMember(p.user_id)}
                  className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-accent/50"
                >
                  <div className="min-w-0 flex-1">
                    <IdentityCrest
                      id={p.user_id}
                      name={p.name || t("common.unnamed")}
                      status={statusFor(p.account_id)}
                      variant="compact"
                    />
                  </div>
                  <UserPlus className="h-4 w-4 shrink-0 text-signal" />
                </button>
              ))}
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
