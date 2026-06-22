import { ShieldCheck } from "lucide-react";
import { cn } from "@/lib/utils";

export interface SafetyNumberProps {
  /** The grouped safety-number / fingerprint string (already grouped, or raw). */
  value: string;
  /** Optional word sequence (the readable safety-number words). */
  words?: string[];
  /** Verified visual state — tints the blocks teal/verified + shows a check. */
  verified?: boolean;
  className?: string;
}

/**
 * SafetyNumber — reusable presentation for a fingerprint / safety number. Renders the
 * value in grouped mono blocks (all crypto/identity is mono) with an optional verified
 * visual state and the readable word sequence. Pure presentation — computation lives
 * elsewhere (e.g. VerifyContactDialog / the backend safety-number command).
 */
export function SafetyNumber({
  value,
  words,
  verified = false,
  className,
}: SafetyNumberProps) {
  return (
    <div className={cn("space-y-3", className)}>
      <code
        data-testid="safety-number"
        className={cn(
          "block break-all rounded-lg border p-3 font-mono text-sm leading-relaxed tracking-wide",
          verified
            ? "border-verified/40 bg-verified/10 text-verified"
            : "bg-muted/40 text-foreground",
        )}
      >
        {value}
      </code>
      {words && words.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {words.map((w, i) => (
            <span
              key={`${w}-${i}`}
              className={cn(
                "inline-flex items-center rounded-md border px-2 py-0.5 font-mono text-xs capitalize",
                verified
                  ? "border-verified/30 text-verified"
                  : "border-border text-muted-foreground",
              )}
            >
              {w}
            </span>
          ))}
        </div>
      )}
      {verified && (
        <p className="flex items-center gap-1.5 text-xs font-medium text-verified">
          <ShieldCheck className="h-3.5 w-3.5" /> Verified
        </p>
      )}
    </div>
  );
}
