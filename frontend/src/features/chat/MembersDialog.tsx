import { useMemo, useState } from "react";
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
import {
  fadeSlideUp,
  listStagger,
  MAX_STAGGER_ITEMS,
  useMotionOK,
} from "@/lib/motion";
import { useAuth } from "@/store/auth";
import { displayName, useChat } from "@/store/chat";
import { OFFLINE, presenceStatus, usePresence } from "@/store/presence";
import { useContactPolicy } from "@/store/contactPolicy";
import { visiblePeers } from "@/lib/contactVisibility";
import { VirtualRosterList, VIRTUALIZE_AT } from "./VirtualRosterList";

// We are always reachable to ourselves, and we never appear in our own discovery roster
// (so no peer/presence entry exists for us) — show the self member as online.
const SELF_ONLINE = presenceStatus({ online: true, last_seen_secs: 0 });

export function MembersDialog() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const members = useChat((s) => s.members);
  const animateMembers = motionOK && members.length <= MAX_STAGGER_ITEMS;
  const channelOwner = useChat((s) => s.channelOwner);
  const peers = useChat((s) => s.peers);
  const hiddenContacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const favorites = useChat((s) => s.favorites);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const myName = useAuth((s) => s.user?.display_name || s.user?.username || "");
  const addMember = useChat((s) => s.addMember);
  const removeMember = useChat((s) => s.removeMember);
  const openConversation = useChat((s) => s.open);
  // Whole presence map (the dialog reads many ids at once; one subscription).
  const presenceMap = usePresence((s) => s.map);
  const [open, setOpen] = useState(false);

  const statusFor = (accountId: string | null | undefined) =>
    presenceStatus((accountId ? presenceMap[accountId] : undefined) ?? OFFLINE);

  const memberIds = useMemo(
    () => new Set(members.map((m) => m.user_id)),
    [members],
  );
  const addable = useMemo(
    () =>
      policyLoaded
        ? visiblePeers(peers, hiddenContacts).filter(
            (p) => !memberIds.has(p.user_id),
          )
        : [],
    [policyLoaded, peers, hiddenContacts, memberIds],
  );
  // Only the channel owner may change membership — the core enforces this (a non-owner's
  // add/remove is rejected by every node), so a non-owner only ever sees a read-only list.
  const isOwner = channelOwner !== "" && channelOwner === myId;

  // Membership retains the verified account id when a device leaves discovery.
  const accountByUserId = useMemo(() => {
    const accounts = new Map<string, string>();
    for (const peer of peers) {
      if (peer.account_id) accounts.set(peer.user_id, peer.account_id);
    }
    for (const member of members) {
      if (member.account_id) accounts.set(member.user_id, member.account_id);
    }
    return accounts;
  }, [members, peers]);

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
          initial={animateMembers ? "hidden" : false}
          animate="visible"
          variants={listStagger}
        >
          <VirtualRosterList
            items={members}
            maxHeight={224}
            rowHeight={58}
            focusSelector="[data-member-row]"
            className="space-y-0.5 overflow-y-auto"
            itemKey={(m) => m.user_id}
            renderItem={(m, index) => {
              // Self isn't in the discovery roster, so it has no peer-derived name/presence:
              // show our own display name and treat ourselves as online.
              const isSelf = m.user_id === myId;
              const accountId = accountByUserId.get(m.user_id) ?? null;
              const name = isSelf
                ? myName || m.name || t("common.unnamed")
                : displayName(favorites, accountId ?? m.user_id, m.name) ||
                  t("common.unnamed");
              const status = isSelf ? SELF_ONLINE : statusFor(accountId);
              // Key the crest by ACCOUNT id, not the device user_id: custom/propagated avatars
              // are stored per account (useAvatar reads account ids), so a device-keyed crest
              // would never resolve a member's photo. Fall back to the device id if the account
              // isn't known from the roster yet. (Also makes the glyph match the member's
              // account-keyed DM glyph.)
              const crestId = isSelf
                ? myAccountId || m.user_id
                : (accountId ?? m.user_id);
              const memberIsOwner = m.user_id === channelOwner;
              // Tapping a member opens a 1:1 chat with them — but only when it's someone else
              // AND we know their account (a DM is account-addressed; an offline member we've
              // never met has no known account, so the row is non-interactive).
              const dmAccount = isSelf ? null : accountId;
              const openDm = () => {
                if (!dmAccount) return;
                openConversation({ kind: "account", id: dmAccount, name });
                setOpen(false);
              };
              return (
                <motion.div
                  key={m.user_id}
                  variants={fadeSlideUp}
                  data-member-row={index}
                  role="group"
                  aria-label={name}
                  tabIndex={members.length > VIRTUALIZE_AT ? 0 : undefined}
                  className="group flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {dmAccount ? (
                    <button
                      type="button"
                      onClick={openDm}
                      data-testid={`member-dm-${m.user_id}`}
                      title={t("members.openDm")}
                      className="min-w-0 flex-1 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <IdentityCrest
                        id={crestId}
                        name={name}
                        status={status}
                        variant="compact"
                      />
                    </button>
                  ) : (
                    <div className="min-w-0 flex-1">
                      <IdentityCrest
                        id={crestId}
                        name={name}
                        status={status}
                        variant="compact"
                      />
                    </div>
                  )}
                  {memberIsOwner && (
                    <span
                      className="shrink-0 rounded-md bg-signal/15 px-2 py-1 text-[11px] font-medium text-signal"
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
                      className="h-11 w-11 shrink-0 text-muted-foreground opacity-0 transition-opacity hover:text-destructive group-hover:opacity-100 group-focus-within:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
                      title={t("common.remove")}
                      aria-label={t("common.remove")}
                      onClick={() => removeMember(m.user_id)}
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  )}
                </motion.div>
              );
            }}
          />
        </motion.div>

        {isOwner && addable.length > 0 && (
          <div className="space-y-1.5">
            <div className="text-[12px] font-semibold text-muted-foreground">
              {t("members.addPeer")}
            </div>
            <VirtualRosterList
              items={addable}
              maxHeight={160}
              className="space-y-0.5 overflow-y-auto rounded-lg border p-1"
              itemKey={(p) => p.user_id}
              renderItem={(p) => (
                <button
                  key={p.user_id}
                  onClick={() => addMember(p.user_id)}
                  className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-accent/50"
                >
                  <div className="min-w-0 flex-1">
                    <IdentityCrest
                      id={p.account_id ?? p.user_id}
                      name={p.name || t("common.unnamed")}
                      status={statusFor(p.account_id)}
                      variant="compact"
                    />
                  </div>
                  <UserPlus className="h-4 w-4 shrink-0 text-signal" />
                </button>
              )}
            />
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
