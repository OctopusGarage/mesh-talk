import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AvatarEditMenu,
  IdentityCrest,
  PresenceDot,
} from "@/components/identity";
import { GroupAvatar } from "@/components/GroupAvatar";
import { chat as chatApi } from "@/lib/api";
import { rosterProjection } from "@/lib/rosterProjection";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import { MemberProfileDialog } from "./MemberProfileDialog";

/** DM header — the large IdentityCrest (glyph + name + presence + verified), with the
 *  safety number reachable via the verify dialog rendered alongside in the header. */
export function DmHeader({ id, name }: { id: string; name: string }) {
  const { t } = useTranslation();
  const [profileOpen, setProfileOpen] = useState(false);
  const owner = useAuth((s) => s.user?.id);
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);

  // Verified state for the crest: a single trust read for the active account (not per-row,
  // so no fan-out), refreshed when the conversation or its presenting device changes.
  const fingerprint =
    rosterProjection(peers).contactForAccount(id)?.user_id ?? "";
  const [trustResult, setTrustResult] = useState<{
    owner: string;
    id: string;
    fingerprint: string;
    verified: boolean;
  } | null>(null);
  const verified =
    trustResult !== null &&
    trustResult.owner === owner &&
    trustResult.id === id &&
    trustResult.fingerprint === fingerprint &&
    trustResult.verified;
  useEffect(() => {
    if (!fingerprint || !owner) return;
    let live = true;
    void chatApi
      .getTrust(owner, id, fingerprint)
      .then(
        (tr) =>
          live &&
          setTrustResult({
            owner,
            id,
            fingerprint,
            verified: tr.verified && !tr.fingerprint_changed,
          }),
      )
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [owner, id, fingerprint]);
  useEffect(() => setProfileOpen(false), [id]);

  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <button
        type="button"
        onClick={() => setProfileOpen(true)}
        data-testid="conversation-member-profile"
        aria-label={t("memberProfile.viewNamed", { name })}
        className="min-w-0 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
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
  memberCount,
  archived,
  dissolved,
  left,
  pendingInvite,
}: {
  id: string;
  name: string;
  memberCount: number;
  archived: boolean;
  dissolved: boolean;
  left: boolean;
  pendingInvite: boolean;
}) {
  const { t } = useTranslation();
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);
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
        {!archived && !dissolved && !left && !pendingInvite && (
          <PresenceDot
            status={status}
            size="md"
            className="absolute -bottom-0.5 -right-0.5"
          />
        )}
      </div>
      <div className="min-w-0">
        <div
          className="truncate font-display font-semibold tracking-tight"
          title={name}
        >
          {name}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {dissolved
            ? t("conversation.dissolvedChannel")
            : archived
              ? t("conversation.archivedChannel")
              : left
                ? t("conversation.leftChannel")
                : pendingInvite
                  ? t("conversation.pendingInvite")
                  : memberCount
                    ? t("conversation.members", { count: memberCount })
                    : t("conversation.channel")}
        </div>
      </div>
    </div>
  );
}
