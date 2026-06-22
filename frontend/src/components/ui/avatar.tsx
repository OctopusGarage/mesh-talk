import { cn } from "@/lib/utils";
import { IdentityGlyph } from "@/components/identity/IdentityGlyph";

/**
 * Avatar — renders an image when `src` is provided, otherwise falls back to the
 * on-brand deterministic IdentityGlyph sigil derived from the id (the "Ink & Signal"
 * signature). Existing `name`/`id`/`className` API is unchanged; `src` is additive.
 */
export function Avatar({
  name,
  id,
  src,
  size = 36,
  verified,
  className,
}: {
  name: string;
  id: string;
  src?: string;
  size?: number;
  verified?: boolean;
  className?: string;
}) {
  if (src) {
    return (
      <img
        src={src}
        alt={name || id}
        width={size}
        height={size}
        className={cn("shrink-0 rounded-full object-cover", className)}
      />
    );
  }
  return (
    <IdentityGlyph
      seed={id || name}
      size={size}
      verified={verified}
      title={name || id}
      className={className}
    />
  );
}
