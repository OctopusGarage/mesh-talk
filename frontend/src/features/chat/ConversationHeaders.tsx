import { captureRuntimeOwner } from "@/store/ownership";
import { useAccountProfile } from "@/store/accountProfiles";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Bookmark } from "lucide-react";
import { AvatarEditMenu, IdentityCrest } from "@/components/identity";
import { GroupAvatar } from "@/components/GroupAvatar";
import { chat as chatApi } from "@/lib/api";
import { rosterProjection } from "@/lib/rosterProjection";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import {
  presenceLabel,
  groupPresenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import { PersonalDevices } from "./PersonalDevices";
import { MemberProfileDialog } from "./MemberProfileDialog";

export function PersonalHeader() {
  const { t } = useTranslation();
  return (
    <div className="flex min-w-0 items-center gap-3">
      <span
        className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-signal/10 text-signal"
        aria-hidden="true"
      >
        <Bookmark className="h-5 w-5" />
      </span>
      <div className="min-w-0">
        <div className="truncate font-display font-semibold tracking-tight">
          {t("personal.title")}
        </div>
        <div className="text-xs text-muted-foreground">
          {t("personal.description")}
        </div>
      </div>
      <PersonalDevices />
    </div>
  );
}

/** DM header — the large IdentityCrest (glyph + name + presence + verified), with the
 *  safety number reachable via the verify dialog rendered alongside in the header. */
export function DmHeader({ id, name }: { id: string; name: string }) {
  const { t } = useTranslation();
  const [profileOpen, setProfileOpen] = useState(false);
  const owner = useAuth((s) => s.user?.id);
  const sessionGeneration = useAuth((s) => s.generation);
  const runtimeScope = useChat((s) => `${s.runEpoch}:${s.identityEpoch}`);
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);

  // Verified state for the crest: a single trust read for the active account (not per-row,
  // so no fan-out), refreshed when the conversation or its presenting device changes.
  const fingerprint =
    rosterProjection(peers).contactForAccount(id)?.user_id ?? "";
  const [trustResult, setTrustResult] = useState<{
    owner: string;
    sessionGeneration: number;
    runtimeScope: string;
    id: string;
    fingerprint: string;
    verified: boolean;
  } | null>(null);
  const verified =
    trustResult !== null &&
    trustResult.owner === owner &&
    trustResult.sessionGeneration === sessionGeneration &&
    trustResult.runtimeScope === runtimeScope &&
    trustResult.id === id &&
    trustResult.fingerprint === fingerprint &&
    trustResult.verified;
  useEffect(() => {
    if (!fingerprint || !owner) return;
    const lease = captureRuntimeOwner();
    let live = true;
    void chatApi
      .getTrust(owner, id, fingerprint)
      .then(
        (tr) =>
          live &&
          lease.current() &&
          setTrustResult({
            owner,
            sessionGeneration,
            runtimeScope,
            id,
            fingerprint,
            verified: tr.verified && !tr.fingerprint_changed,
          }),
      )
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [owner, sessionGeneration, runtimeScope, id, fingerprint]);
  useEffect(() => setProfileOpen(false), [owner, sessionGeneration, id]);

  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <button
        type="button"
        onClick={() => setProfileOpen(true)}
        data-testid="conversation-member-profile"
        title={t("memberProfile.viewNamed", { name })}
        className="min-w-0 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="sr-only">{t("memberProfile.view")}</span>
        <IdentityCrest
          id={id}
          name={name}
          verified={verified}
          hideId
          status={status}
          variant="compact"
        />
        <span className="block pl-12 text-[11px] text-muted-foreground">
          {presenceLabel(presence, t)}
        </span>
      </button>
      <MemberProfileDialog
        target={profileOpen ? { name, accountId: id } : null}
        onOpenChange={setProfileOpen}
      />
    </div>
  );
}

/** Channel header — a mesh/group glyph + name + member count. */
export function ChannelHeader({
  id,
  name,
  archived,
  dissolved,
  left,
  pendingInvite,
  sharedReference = false,
  previousAccount = false,
  memberCount,
}: {
  id: string;
  name: string;
  memberCount: number;
  sharedReference?: boolean;
  previousAccount?: boolean;
  archived: boolean;
  dissolved: boolean;
  left: boolean;
  pendingInvite: boolean;
}) {
  const { t } = useTranslation();
  const presence = usePresenceFor(id);
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div className="relative">
        <AvatarEditMenu
          id={id}
          ariaLabel={t("avatar.editGroup")}
          category="group"
        >
          <GroupAvatar channelId={id} size={36} title={name} />
        </AvatarEditMenu>
      </div>
      <div className="min-w-0">
        <div
          className="truncate font-display font-semibold tracking-tight"
          title={name}
        >
          {name}
        </div>
        <div
          className="text-xs text-muted-foreground"
          title={
            sharedReference || previousAccount
              ? undefined
              : t("presence.knownOnlineHint")
          }
        >
          {previousAccount
            ? t("conversation.previousAccountChannel")
            : sharedReference
              ? `${t("conversation.sharedReference")} · ${t("conversation.members", { count: memberCount })}`
              : dissolved
                ? t("conversation.dissolvedChannel")
                : archived
                  ? t("conversation.archivedChannel")
                  : left
                    ? t("conversation.leftChannel")
                    : pendingInvite
                      ? t("conversation.pendingInvite")
                      : groupPresenceLabel(presence, t)}
        </div>
      </div>
    </div>
  );
}

/** The public About text occupies available header space without crowding actions. */
export function ConversationBio({ accountId }: { accountId: string }) {
  const profile = useAccountProfile(accountId);
  const bio = profile?.bio?.trim();
  return (
    <div className="min-w-0 flex-1">
      {bio && (
        <p
          data-testid="conversation-bio"
          className="truncate text-xs text-muted-foreground"
          title={bio}
        >
          {bio}
        </p>
      )}
    </div>
  );
}
