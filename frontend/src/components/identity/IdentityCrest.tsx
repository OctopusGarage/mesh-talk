import { ShieldCheck } from "lucide-react";
import { cn } from "@/lib/utils";
import { IdentityGlyph } from "./IdentityGlyph";
import { AvatarEditMenu } from "./AvatarEditMenu";
import { PresenceDot, type PresenceStatus } from "./PresenceDot";

/** First 4 + last 4 of a fingerprint, grouped, for compact display. */
function crestId(id: string): string {
  const s = (id || "").replace(/\s+/g, "");
  if (s.length <= 8) return s;
  return `${s.slice(0, 4)} … ${s.slice(-4)}`;
}

export interface IdentityCrestProps {
  /** Fingerprint / user id — drives the glyph and short id. */
  id: string;
  /** Display name. */
  name: string;
  verified?: boolean;
  /** Optional presence; when set, a PresenceDot overlays the glyph. */
  status?: PresenceStatus;
  variant?: "compact" | "large";
  /** When set, the glyph becomes an editable avatar (set/remove photo) keyed by this id. */
  editAvatarId?: string;
  /** Accessible label for the avatar-edit button (required when `editAvatarId` is set). */
  editAvatarLabel?: string;
  className?: string;
}

/**
 * IdentityCrest — the composed signature: IdentityGlyph + display name (display font)
 * + a short mono id + optional verified badge. `compact` for list rows / headers,
 * `large` for profile/verify panels.
 */
export function IdentityCrest({
  id,
  name,
  verified = false,
  status,
  variant = "compact",
  editAvatarId,
  editAvatarLabel,
  className,
}: IdentityCrestProps) {
  const large = variant === "large";
  const glyphSize = large ? 56 : 36;
  const glyph = (
    <IdentityGlyph
      seed={id}
      size={glyphSize}
      verified={verified}
      title={name}
    />
  );
  return (
    <div className={cn("flex items-center gap-3", className)}>
      <div className="relative">
        {editAvatarId ? (
          <AvatarEditMenu
            id={editAvatarId}
            ariaLabel={editAvatarLabel ?? name}
            category="personal"
          >
            {glyph}
          </AvatarEditMenu>
        ) : (
          glyph
        )}
        {status && (
          <PresenceDot
            status={status}
            size={large ? "lg" : "md"}
            className="pointer-events-none absolute -bottom-0.5 -right-0.5"
          />
        )}
      </div>
      <div className="min-w-0">
        <div className="flex items-center gap-1.5">
          <span
            className={cn(
              "truncate font-display font-semibold tracking-tight",
              large ? "text-lg" : "text-sm",
            )}
          >
            {name}
          </span>
          {verified && (
            <ShieldCheck
              className={cn(
                "shrink-0 text-verified",
                large ? "h-4 w-4" : "h-3.5 w-3.5",
              )}
              aria-label="verified"
            />
          )}
        </div>
        <span
          className={cn(
            "block truncate font-mono text-muted-foreground",
            large ? "text-sm" : "text-xs",
          )}
        >
          {crestId(id)}
        </span>
      </div>
    </div>
  );
}
