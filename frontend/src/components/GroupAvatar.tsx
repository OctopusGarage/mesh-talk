import { Network } from "lucide-react";
import { IdentityGlyph } from "@/components/identity";
import { useChat } from "@/store/chat";
import { useAvatar } from "@/store/avatars";
import { rosterProjection } from "@/lib/rosterProjection";
import { channelMemberAccounts } from "@/features/chat/channelMemberProjection";
import type { ChannelMemberInfo } from "@/lib/types";

const NO_MEMBERS: ChannelMemberInfo[] = [];

/**
 * A WeChat-style group avatar: a square montage of up to 9 account identity glyphs/avatars,
 * derived LIVE from the channel's current members when no private/shared photo exists.
 * Each tile is an account's custom avatar if they set
 * one, else their deterministic IdentityGlyph. Falls back to a mesh sigil until members load.
 */
export function GroupAvatar({
  channelId,
  size = 36,
  title,
}: {
  channelId: string;
  size?: number;
  title?: string;
}) {
  const members = useChat((s) => s.channelMembersById[channelId] ?? NO_MEMBERS);
  const peers = useChat((s) => s.peers);
  const roster = rosterProjection(peers);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);

  // A photo explicitly set for this channel (e.g. a club logo from the gallery) wins over
  // the auto-composite — IdentityGlyph(channelId) renders that avatar.
  const custom = useAvatar(channelId);
  const shared = useChat(
    (s) =>
      s.channels.find((channel) => channel.channel_id === channelId)?.metadata
        ?.cover?.content,
  );
  const photo = custom ?? shared;
  if (photo) {
    return (
      <img
        src={photo}
        width={size}
        height={size}
        alt={title ?? ""}
        title={title}
        className="shrink-0 rounded-[28%] object-cover"
      />
    );
  }

  // Match the header's accepted-account count; invitations remain in the member
  // dialog without adding people to the default avatar.
  const seeds = channelMemberAccounts(
    members.filter((member) => !member.pending),
    roster,
    { deviceId: myId, accountId: myAccountId, ownerId: "" },
  )
    .slice(0, 9)
    .map((account) => account.accountId ?? account.representative.user_id);

  if (seeds.length === 0) {
    return (
      <div
        className="flex shrink-0 items-center justify-center rounded-[28%] border border-border bg-secondary text-muted-foreground"
        style={{ width: size, height: size }}
        title={title}
        aria-hidden
      >
        <Network style={{ width: size * 0.5, height: size * 0.5 }} />
      </div>
    );
  }

  const cols = Math.ceil(Math.sqrt(seeds.length));
  const cell = Math.max(8, Math.floor((size - (cols + 1)) / cols));
  return (
    <div
      data-testid="group-avatar"
      className="grid shrink-0 place-content-center gap-px overflow-hidden rounded-[28%] bg-secondary"
      style={{
        width: size,
        height: size,
        gridTemplateColumns: `repeat(${cols}, ${cell}px)`,
      }}
      title={title}
      aria-hidden
    >
      {seeds.map((seed) => (
        <IdentityGlyph
          key={seed}
          seed={seed}
          size={cell}
          className="!rounded-none"
        />
      ))}
    </div>
  );
}
