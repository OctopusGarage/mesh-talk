import { GroupControlStatus, PendingGroupControls } from "./GroupControlStatus";
import { useGroupControlOperation } from "./useGroupControlOperation";
import { canManageChannel, canParticipateChannel } from "./channelPermissions";
import {
  channelMemberAccounts,
  channelInviteAccounts,
} from "./channelMemberProjection";
import { displayIdentity } from "@/lib/displayIdentity";
import { useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";
import {
  ArrowLeft,
  Crown,
  LogOut,
  MessageCircle,
  Settings,
  Users,
  UserPlus,
  X,
} from "lucide-react";
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
import { useChat } from "@/store/chat";
import { chat as chatApi } from "@/lib/api";
import type { ChannelActivityInfo, ChannelMemberInfo } from "@/lib/types";
import {
  OFFLINE,
  presenceLabel,
  presenceStatus,
  usePresence,
} from "@/store/presence";
import { useContactPolicy } from "@/store/contactPolicy";
import { peerAccountKey, rosterProjection } from "@/lib/rosterProjection";
import { visiblePeers } from "@/lib/contactVisibility";
import { VirtualRosterList, VIRTUALIZE_AT } from "./VirtualRosterList";
import { GroupPreferences } from "./GroupPreferences";
import { MemberProfileDialog } from "./MemberProfileDialog";
import type { MemberProfileTarget } from "./memberProfileDetails";

export function MembersDialog({ dissolved = false }: { dissolved?: boolean }) {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const storedMembers = useChat((s) => s.members);
  const storedOwner = useChat((s) => s.channelOwner);
  const active = useChat((s) => s.active);
  const channelId = active?.kind === "channel" ? active.id : null;
  const [liveMembers, setLiveMembers] = useState<{
    channelId: string;
    baseline: { members: ChannelMemberInfo[]; owner: string };
    members: ChannelMemberInfo[];
    owner: string;
    can_manage?: boolean;
    account_access?: import("@/lib/types").AccountGroupAccessInfo | null;
    owner_account_id?: string | null;
  } | null>(null);
  const storedSnapshotRef = useRef({
    members: storedMembers,
    owner: storedOwner,
  });
  useEffect(() => {
    storedSnapshotRef.current = { members: storedMembers, owner: storedOwner };
  }, [storedMembers, storedOwner]);
  const liveSnapshot =
    active?.kind === "channel" &&
    liveMembers?.channelId === active.id &&
    liveMembers.baseline.members === storedMembers &&
    liveMembers.baseline.owner === storedOwner
      ? liveMembers
      : null;
  const members = liveSnapshot?.members ?? storedMembers;
  const channelOwner = liveSnapshot?.owner ?? storedOwner;
  const channel = useChat((s) =>
    s.channels?.find((item) => item.channel_id === channelId),
  );
  const sharedReference = channel?.shared_reference === true;
  const accountAccess =
    liveSnapshot?.account_access !== undefined
      ? liveSnapshot.account_access
      : channel?.account_access;
  const referenceOnly =
    sharedReference && (!accountAccess || accountAccess.role === "unknown");
  const authority = {
    ...channel,
    owner: channelOwner,
    account_access: accountAccess,
    can_manage: liveSnapshot?.can_manage ?? channel?.can_manage,
    owner_account_id:
      (liveSnapshot?.owner_account_id !== undefined
        ? liveSnapshot.owner_account_id
        : channel?.owner_account_id) ??
      (accountAccess?.role === "owner" ? accountAccess.account_id : null),
  };
  const peers = useChat((s) => s.peers);
  const accounts = useChat((s) => s.accounts);
  const roster = rosterProjection(peers);
  const hiddenContacts = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const favorites = useChat((s) => s.favorites);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const myName = useAuth((s) => s.user?.display_name || s.user?.username || "");
  const localOwner = useAuth((s) => s.user?.id);
  const authGeneration = useAuth((s) => s.generation);
  const runtimeScope = useChat((s) => `${s.runEpoch}:${s.identityEpoch}`);
  const addMember = useChat((s) => s.addMember);
  const removeMember = useChat((s) => s.removeMember);
  const removeMemberAccount = useChat((s) => s.removeMemberAccount);
  const dissolveChannel = useChat((s) => s.dissolveChannel);
  const transferChannelOwner = useChat((s) => s.transferChannelOwner);
  const leaveChannel = useChat((s) => s.leaveChannel);
  const openConversation = useChat((s) => s.open);
  // Whole presence map (the dialog reads many ids at once; one subscription).
  const presenceMap = usePresence((s) => s.map);
  const [open, setOpen] = useState(false);
  const [profileTarget, setProfileTarget] =
    useState<MemberProfileTarget | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const operation = useGroupControlOperation(channelId);
  const busy = operation.busy;
  const [pendingRemoval, setPendingRemoval] = useState<{
    channelId: string;
    id: string;
    name: string;
    account: boolean;
  } | null>(null);
  const removing = busy;
  const [pendingTransfer, setPendingTransfer] = useState<{
    channelId: string;
    id: string;
    name: string;
  } | null>(null);
  const transferring = busy;
  const [confirmLeave, setConfirmLeave] = useState<string | null>(null);
  const leaving = busy;
  const [activitySnapshot, setActivitySnapshot] = useState<{
    channelId: string;
    entries: ChannelActivityInfo[];
  } | null>(null);
  const activity =
    activitySnapshot?.channelId === channelId ? activitySnapshot.entries : [];
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsTriggerRef = useRef<HTMLButtonElement>(null);
  const membersBackRef = useRef<HTMLButtonElement>(null);
  const wasSettingsOpen = useRef(false);
  useEffect(() => {
    if (!open) {
      wasSettingsOpen.current = false;
      return;
    }
    if (settingsOpen) membersBackRef.current?.focus();
    else if (wasSettingsOpen.current) settingsTriggerRef.current?.focus();
    wasSettingsOpen.current = settingsOpen;
  }, [open, settingsOpen]);
  const ownMember = members.find((member) => member.user_id === myId);
  useEffect(() => {
    setSettingsOpen(false);
    setLiveMembers(null);
    setProfileTarget(null);
    setPendingRemoval(null);
    setPendingTransfer(null);
    setConfirming(null);
    setConfirmLeave(null);
    setActivitySnapshot(null);
  }, [active?.id, sharedReference]);
  useEffect(() => {
    if (!open || active?.kind !== "channel") return;
    let live = true;
    const baseline = storedSnapshotRef.current;
    void chatApi.channelMembers(active.id).then(
      (info) => {
        if (live)
          setLiveMembers({
            channelId: active.id,
            baseline,
            members: info.members,
            owner: info.owner,
            can_manage: info.can_manage,
            account_access: info.account_access,
            owner_account_id: info.owner_account_id,
          });
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [open, active?.id, active?.kind, sharedReference, settingsOpen]);
  useEffect(() => {
    if (!open || active?.kind !== "channel") return;
    let live = true;
    const requestedChannel = active.id;
    void chatApi.channelActivity(requestedChannel).then(
      (items) => {
        if (live)
          setActivitySnapshot({ channelId: requestedChannel, entries: items });
      },
      () => {
        if (live)
          setActivitySnapshot({ channelId: requestedChannel, entries: [] });
      },
    );
    return () => {
      live = false;
    };
  }, [open, active?.id, active?.kind, members, channelOwner]);

  const confirmRemoval = async () => {
    if (!pendingRemoval || pendingRemoval.channelId !== channelId || removing)
      return;
    await operation.run(
      () =>
        (pendingRemoval.account ? removeMemberAccount : removeMember)(
          pendingRemoval.id,
        ),
      () => setPendingRemoval(null),
    );
  };
  const confirmTransfer = async () => {
    if (
      !pendingTransfer ||
      pendingTransfer.channelId !== channelId ||
      transferring
    )
      return;
    await operation.run(
      () => transferChannelOwner(pendingTransfer.id),
      () => setPendingTransfer(null),
    );
  };
  const finishLeave = async () => {
    if (confirmLeave !== channelId || leaving) return;
    await operation.run(leaveChannel, () => {
      setConfirmLeave(null);
      setOpen(false);
    });
  };

  const statusFor = (accountId: string | null | undefined) =>
    presenceStatus((accountId ? presenceMap[accountId] : undefined) ?? OFFLINE);

  const memberIds = useMemo(
    () => new Set(members.map((m) => m.user_id)),
    [members],
  );
  const memberAccounts = useMemo(
    () =>
      channelMemberAccounts(members, roster, {
        deviceId: myId,
        accountId: myAccountId,
        ownerId: channelOwner,
      }),
    [members, roster, myId, myAccountId, channelOwner],
  );
  const animateMembers = motionOK && memberAccounts.length <= MAX_STAGGER_ITEMS;
  const addableDevices = useMemo(
    () =>
      policyLoaded
        ? visiblePeers(peers, hiddenContacts).filter(
            (peer) => !memberIds.has(peer.user_id),
          )
        : [],
    [policyLoaded, peers, hiddenContacts, memberIds],
  );
  const memberAccountIds = new Set(
    memberAccounts.flatMap((group) =>
      group.accountId ? [group.accountId] : [],
    ),
  );
  const addable = channelInviteAccounts(
    addableDevices.filter(
      (peer) => !!peer.account_id && !memberAccountIds.has(peer.account_id),
    ),
  );
  // The native channel authority includes certified devices of the owner account.
  // Roster bindings and display names never grant management rights.
  const isOwner = !dissolved && canManageChannel(authority, myId);

  if (confirming && confirming === channelId && isOwner) {
    return (
      <Dialog
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          if (!next) setConfirming(null);
        }}
      >
        <DialogContent
          className="max-w-sm"
          data-testid="dissolve-channel-dialog"
        >
          <DialogHeader>
            <DialogTitle>{t("members.dissolve")}</DialogTitle>
            <DialogDescription>
              {t("members.dissolveConfirm")}
            </DialogDescription>
          </DialogHeader>
          <GroupControlStatus
            outcome={operation.outcome}
            cancelRequested={operation.cancelRequested}
          />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirming(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              data-testid="dissolve-channel-confirm"
              onClick={() => {
                if (active?.kind !== "channel" || active.id !== confirming)
                  return;
                void operation.run(
                  () => dissolveChannel(active.id),
                  () => {
                    setOpen(false);
                    setConfirming(null);
                  },
                );
              }}
            >
              {t("members.dissolve")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    );
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (removing || transferring || leaving) return;
        setOpen(next);
        if (!next) {
          setProfileTarget(null);
          setPendingRemoval(null);
          setPendingTransfer(null);
          setConfirmLeave(null);
          setSettingsOpen(false);
        }
      }}
    >
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
          <DialogTitle className="flex min-w-0 flex-wrap items-baseline gap-2">
            {settingsOpen ? t("members.groupSettings") : t("members.title")}
            {!settingsOpen && (
              <span className="font-display text-base font-normal tabular-nums text-muted-foreground">
                {channelId && presenceMap[channelId]?.member_count != null
                  ? t("presence.groupSummary", {
                      total: presenceMap[channelId].member_count,
                      online: presenceMap[channelId].online_count ?? 0,
                    })
                  : sharedReference
                    ? t("conversation.members", {
                        count: channel?.member_count ?? 0,
                      })
                    : t("presence.groupUpdating")}
              </span>
            )}
          </DialogTitle>
          <DialogDescription
            className={settingsOpen || referenceOnly ? undefined : "sr-only"}
          >
            {settingsOpen
              ? active?.name
              : t(
                  referenceOnly
                    ? "conversation.sharedReference"
                    : "members.description",
                )}
          </DialogDescription>
          {!settingsOpen &&
            sharedReference &&
            channel?.reference_members_complete === false && (
              <p className="text-xs text-muted-foreground">
                {t("members.referencePartial")}
              </p>
            )}
        </DialogHeader>

        <GroupControlStatus
          outcome={operation.outcome}
          cancelRequested={operation.cancelRequested}
        />
        {channelId && (
          <PendingGroupControls group={channelId} excludeId={operation.id} />
        )}
        {settingsOpen ? (
          <>
            <Button
              variant="ghost"
              className="justify-start"
              ref={membersBackRef}
              data-testid="group-members-back"
              onClick={() => {
                setSettingsOpen(false);
                setConfirmLeave(null);
              }}
            >
              <ArrowLeft className="h-4 w-4" />
              {t("members.backToMembers")}
            </Button>
            {channelId && (
              <GroupPreferences
                key={`${channelId}:${localOwner}:${authGeneration}:${runtimeScope}:${myId}`}
                channelId={channelId}
                myId={myId}
                channelOwner={channelOwner}
                dissolved={dissolved}
                sharedReference={sharedReference}
                referenceOnly={referenceOnly}
                canManage={isOwner}
                ownMember={ownMember}
                selfAccepted={
                  accountAccess !== undefined
                    ? accountAccess?.role === "owner" ||
                      accountAccess?.role === "member"
                    : members.some(
                        (member) => member.user_id === myId && !member.pending,
                      )
                }
              />
            )}
          </>
        ) : (
          <>
            <motion.div
              initial={animateMembers ? "hidden" : false}
              animate="visible"
              variants={listStagger}
            >
              <VirtualRosterList
                items={memberAccounts}
                maxHeight={224}
                rowHeight={58}
                focusSelector="[data-member-row]"
                className="space-y-0.5 overflow-y-auto"
                itemKey={(group) => group.key}
                renderItem={(group, index) => {
                  const m = group.representative;
                  // Self presence uses the same expiring runtime snapshot as other accounts.
                  const accountId = group.accountId;
                  const isSelf = accountId
                    ? accountId === myAccountId
                    : m.user_id === myId;
                  const identity = displayIdentity({
                    accountId,
                    deviceId: m.user_id,
                    fallbackName: m.name,
                    accounts,
                    peers,
                    favorites,
                  });
                  const name =
                    identity.alias ||
                    (isSelf &&
                    !accounts.some(
                      (account) =>
                        account.account_id === accountId &&
                        account.names.length,
                    )
                      ? myName || identity.name
                      : identity.name);
                  const memberPresence =
                    presenceMap[
                      isSelf
                        ? myAccountId || ""
                        : accountId || `device:${m.user_id}`
                    ] ?? OFFLINE;
                  const status = presenceStatus(memberPresence);
                  // Key the crest by ACCOUNT id, not the device user_id: custom/propagated avatars
                  // are stored per account (useAvatar reads account ids), so a device-keyed crest
                  // would never resolve a member's photo. Fall back to the device id if the account
                  // isn't known from the roster yet. (Also makes the glyph match the member's
                  // account-keyed DM glyph.)
                  const crestId = isSelf
                    ? myAccountId || m.user_id
                    : (accountId ?? m.user_id);
                  const memberIsOwner = authority.owner_account_id
                    ? group.accountId === authority.owner_account_id
                    : group.devices.some(
                        (device) => device.user_id === channelOwner,
                      );
                  // A DM needs a known account. The identity itself remains inspectable even
                  // for an offline member with only a device ID.
                  const dmAccount = isSelf ? null : accountId;
                  const openDm = () => {
                    if (!dmAccount) return;
                    openConversation({ kind: "account", id: dmAccount, name });
                    setOpen(false);
                  };
                  return (
                    <motion.div
                      key={group.key}
                      variants={fadeSlideUp}
                      data-member-row={index}
                      role="group"
                      aria-label={name}
                      tabIndex={
                        memberAccounts.length > VIRTUALIZE_AT ? 0 : undefined
                      }
                      className="group rounded-lg px-2 py-1.5 hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    >
                      <div className="flex items-center gap-3">
                        <button
                          type="button"
                          onClick={() =>
                            setProfileTarget({
                              name,
                              canonicalName: identity.canonicalName,
                              accountId:
                                accountId ?? (isSelf ? myAccountId : null),
                              deviceId: m.user_id,
                              isSelf,
                            })
                          }
                          data-testid={`member-profile-${m.user_id}`}
                          title={t("memberProfile.view")}
                          className="min-w-0 flex-1 rounded-lg text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
                        >
                          <span className="sr-only">
                            {t("memberProfile.view")}
                          </span>
                          <IdentityCrest
                            id={crestId}
                            name={name}
                            status={status}
                            variant="compact"
                            hideId
                          />
                          <span
                            className="block pl-12 text-xs text-muted-foreground"
                            title={t("presence.offlineHint")}
                          >
                            {presenceLabel(memberPresence, t)}
                          </span>
                        </button>
                        {dmAccount && (
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon"
                            onClick={openDm}
                            data-testid={`member-dm-${m.user_id}`}
                            title={t("members.openDm")}
                            aria-label={t("members.openDm")}
                            className="h-11 w-11 shrink-0 max-[400px]:hidden"
                          >
                            <MessageCircle className="h-4 w-4" />
                          </Button>
                        )}
                        {memberIsOwner && (
                          <span
                            className="shrink-0 rounded-md bg-signal/15 px-2 py-1 text-[11px] font-medium text-signal"
                            data-testid="member-owner-badge"
                          >
                            {t("members.owner")}
                          </span>
                        )}
                        {group.devices.every((member) => member.pending) && (
                          <span className="text-xs text-muted-foreground">
                            {t("members.pendingAcceptance")}
                          </span>
                        )}
                        {isOwner && !memberIsOwner && !group.accountId && (
                          <Button
                            variant="ghost"
                            size="icon"
                            title={t("common.remove")}
                            aria-label={`${t("common.remove")} ${name}`}
                            disabled={busy}
                            onClick={() =>
                              channelId &&
                              setPendingRemoval({
                                channelId,
                                id: m.user_id,
                                name,
                                account: false,
                              })
                            }
                          >
                            <X className="h-4 w-4" />
                          </Button>
                        )}
                        {isOwner &&
                          !memberIsOwner &&
                          group.accountId &&
                          group.devices.some((device) => !device.pending) && (
                            <Button
                              variant="ghost"
                              size="icon"
                              className="h-11 w-11 shrink-0 text-muted-foreground"
                              title={t("members.transferOwner")}
                              aria-label={`${t("members.transferOwner")} ${name}`}
                              data-testid={`member-transfer-account-${group.accountId}`}
                              disabled={busy}
                              onClick={() =>
                                channelId &&
                                setPendingTransfer({
                                  channelId,
                                  id: group.accountId!,
                                  name,
                                })
                              }
                            >
                              <Crown className="h-4 w-4" />
                            </Button>
                          )}
                        {isOwner && !memberIsOwner && group.accountId && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="h-11 w-11 shrink-0 text-muted-foreground hover:text-destructive"
                            title={t("common.remove")}
                            aria-label={`${t("common.remove")} ${name}`}
                            data-testid={`member-remove-account-${group.accountId}`}
                            disabled={removing || transferring}
                            onClick={() =>
                              channelId &&
                              setPendingRemoval({
                                channelId,
                                id: group.accountId!,
                                name,
                                account: true,
                              })
                            }
                          >
                            <X className="h-4 w-4" />
                          </Button>
                        )}
                      </div>
                    </motion.div>
                  );
                }}
              />
            </motion.div>

            {!referenceOnly && pendingRemoval?.channelId === channelId && (
              <div className="space-y-3 rounded-lg border border-destructive/30 p-3">
                <p className="break-words text-sm" role="status">
                  {t("members.confirmRemove", { name: pendingRemoval.name })}
                </p>
                <div className="flex justify-end gap-2">
                  <Button
                    variant="ghost"
                    disabled={removing}
                    onClick={() => setPendingRemoval(null)}
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    variant="destructive"
                    disabled={removing}
                    onClick={() => void confirmRemoval()}
                  >
                    {t("members.removeMember")}
                  </Button>
                </div>
              </div>
            )}
            {!referenceOnly && pendingTransfer?.channelId === channelId && (
              <div className="space-y-3 rounded-lg border p-3">
                <p className="break-words text-sm" role="status">
                  {t("members.confirmTransfer", { name: pendingTransfer.name })}
                </p>
                <div className="flex justify-end gap-2">
                  <Button
                    variant="ghost"
                    disabled={transferring}
                    onClick={() => setPendingTransfer(null)}
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    disabled={transferring}
                    onClick={() => void confirmTransfer()}
                  >
                    {t("members.transferOwner")}
                  </Button>
                </div>
              </div>
            )}

            {isOwner && addable.length > 0 && (
              <details className="space-y-1.5">
                <summary className="cursor-pointer py-2 text-sm font-medium">
                  {t("members.addPeer")}
                </summary>
                <VirtualRosterList
                  items={addable}
                  maxHeight={160}
                  className="space-y-0.5 overflow-y-auto rounded-lg border p-1"
                  itemKey={(p) => p.user_id}
                  renderItem={(p) => (
                    <button
                      key={p.user_id}
                      disabled={busy}
                      onClick={() =>
                        void operation.run(() => addMember(p.user_id))
                      }
                      data-testid={`member-invite-${p.user_id}`}
                      className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-accent/50"
                    >
                      <div className="min-w-0 flex-1">
                        <IdentityCrest
                          id={peerAccountKey(p)}
                          name={
                            displayIdentity({
                              accountId: p.account_id,
                              deviceId: p.user_id,
                              fallbackName: p.name,
                              accounts,
                              peers,
                              favorites,
                            }).name
                          }
                          status={statusFor(p.account_id)}
                          variant="compact"
                        />
                      </div>
                      <UserPlus className="h-4 w-4 shrink-0 text-signal" />
                    </button>
                  )}
                />
              </details>
            )}

            <Button
              variant="ghost"
              className="w-full justify-start"
              ref={settingsTriggerRef}
              data-testid="group-settings-trigger"
              onClick={() => {
                setSettingsOpen(true);
                setProfileTarget(null);
                setPendingRemoval(null);
                setPendingTransfer(null);
              }}
            >
              <Settings className="h-4 w-4" />
              {t("members.groupSettings")}
            </Button>
          </>
        )}
        {settingsOpen && activity.length > 0 && (
          <details className="space-y-2 border-t pt-3">
            <summary className="cursor-pointer py-1 text-sm font-medium">
              {t("members.activity")}
            </summary>
            <ol className="max-h-36 space-y-1.5 overflow-y-auto text-xs text-muted-foreground">
              {activity
                .slice(-20)
                .reverse()
                .map((entry, index) => {
                  const date = new Date(entry.wall_clock);
                  const dateValid = Number.isFinite(date.getTime());
                  const nameFor = (id: string) => {
                    const member = members.find(
                      (entry) => entry.user_id === id,
                    );
                    return displayIdentity({
                      accountId: roster.accountForMember(
                        member ?? { user_id: id },
                      ),
                      deviceId: id,
                      fallbackName: member?.name,
                      accounts,
                      peers,
                      favorites,
                    }).name;
                  };
                  return (
                    <li
                      key={`${entry.wall_clock}-${index}`}
                      className="flex justify-between gap-3"
                    >
                      <span className="min-w-0 break-words">
                        {t(`members.activity_${entry.kind}`, {
                          actor: nameFor(entry.actor),
                          target: entry.target ? nameFor(entry.target) : "",
                        })}
                      </span>
                      <time
                        className="shrink-0 tabular-nums"
                        dateTime={dateValid ? date.toISOString() : undefined}
                      >
                        {dateValid ? date.toLocaleDateString() : ""}
                      </time>
                    </li>
                  );
                })}
            </ol>
          </details>
        )}
        {!settingsOpen && (
          <>
            {isOwner && active?.kind === "channel" && (
              <Button
                variant="ghost"
                className="text-destructive hover:text-destructive"
                data-testid="dissolve-channel-trigger"
                onClick={() => setConfirming(channelId)}
              >
                {t("members.dissolve")}
              </Button>
            )}
            <div className="border-t pt-3">
              {referenceOnly ? (
                <p className="text-xs text-muted-foreground">
                  {t(
                    channel?.legacy_membership
                      ? "conversation.legacySharedReferenceHint"
                      : "conversation.sharedReferenceHint",
                  )}
                </p>
              ) : dissolved ? (
                <p className="text-xs text-muted-foreground">
                  {t("conversation.dissolvedChannelHint")}
                </p>
              ) : isOwner ? (
                <p className="text-xs text-muted-foreground">
                  {t("members.transferBeforeLeave")}
                </p>
              ) : accountAccess !== undefined &&
                !canParticipateChannel(authority) ? null : channelId !== null &&
                confirmLeave === channelId ? (
                <div className="space-y-3">
                  <p className="text-sm" role="status">
                    {t("members.confirmLeave")}
                  </p>
                  <div className="flex justify-end gap-2">
                    <Button
                      variant="ghost"
                      disabled={leaving}
                      onClick={() => setConfirmLeave(null)}
                    >
                      {t("common.cancel")}
                    </Button>
                    <Button
                      variant="destructive"
                      disabled={leaving}
                      onClick={() => void finishLeave()}
                    >
                      {t("members.leave")}
                    </Button>
                  </div>
                </div>
              ) : (
                <Button
                  variant="ghost"
                  className="text-destructive hover:text-destructive"
                  onClick={() => setConfirmLeave(channelId)}
                >
                  <LogOut className="h-4 w-4" />
                  {t("members.leave")}
                </Button>
              )}
            </div>
          </>
        )}
        <MemberProfileDialog
          target={profileTarget}
          onOpenChange={(next) => {
            if (!next) setProfileTarget(null);
          }}
          onMessage={(accountId, name) => {
            openConversation({ kind: "account", id: accountId, name });
            setOpen(false);
            setProfileTarget(null);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
