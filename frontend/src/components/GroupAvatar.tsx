import { Network } from "lucide-react";
import { IdentityGlyph } from "@/components/identity";
import { useChat } from "@/store/chat";
import { useAvatar } from "@/store/avatars";
import type { ChannelMemberInfo } from "@/lib/types";

const NO_MEMBERS: ChannelMemberInfo[] = [];

/**
 * A WeChat-style group avatar: a square montage of up to 9 member identity glyphs/avatars,
 * derived LIVE from the channel's current members (no stored group photo, no protocol — it
 * always reflects who's in the channel). Each tile is a member's custom avatar if they set
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
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);

  // A photo explicitly set for this channel (e.g. a club logo from the gallery) wins over
  // the auto-composite — IdentityGlyph(channelId) renders that avatar.
  const custom = useAvatar(channelId);
  if (custom) {
    return <IdentityGlyph seed={channelId} size={size} title={title} />;
  }

  // A member's avatar SEED is its account id (avatars are account-keyed): self resolves to
  // our account, others via the roster; fall back to the device id when the account's unknown.
  const seeds = members
    .slice(0, 9)
    .map((m) =>
      m.user_id === myId
        ? myAccountId || m.user_id
        : (peers.find((p) => p.user_id === m.user_id)?.account_id ?? m.user_id),
    );

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
      {seeds.map((seed, i) => (
        <IdentityGlyph
          key={`${seed}-${i}`}
          seed={seed}
          size={cell}
          className="!rounded-none"
        />
      ))}
    </div>
  );
}
