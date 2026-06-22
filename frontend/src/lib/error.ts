// Backend IPC commands reject with a tagged CommandError ({ kind, message }) — see
// src-tauri/src/commands.rs. These helpers read it safely, falling back for plain
// string / Error rejections.

// Mirrors the Rust `CommandError` `kind` taxonomy (src-tauri/src/commands.rs). The
// legacy single-word kinds ("validation" / "authentication" / "service" / "network") are
// kept here only so older/handwritten rejections still type-check and route sensibly.
export type CommandErrorKind =
  | "peer-unknown"
  | "relay-unreachable"
  | "crypto"
  | "auth"
  | "authorization"
  | "io"
  | "not-started"
  | "invalid-input"
  | "internal"
  // legacy kinds (pre-taxonomy) — still accepted on the wire
  | "validation"
  | "authentication"
  | "service"
  | "network";

export interface CommandError {
  kind: CommandErrorKind;
  message: string;
}

export function isCommandError(e: unknown): e is CommandError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as Record<string, unknown>).message === "string"
  );
}

/** A human-readable message for any rejection (CommandError, Error, string, or unknown). */
export function errorMessage(e: unknown): string {
  if (isCommandError(e)) return e.message;
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "Something went wrong";
}

/** The error kind, when the rejection is a structured CommandError. */
export function errorKind(e: unknown): CommandErrorKind | null {
  return isCommandError(e) ? e.kind : null;
}

// Coarse reason for a failed outgoing message. The backend now ships a structured
// CommandError `kind`, so we map from THAT first; keyword-sniffing the message is only a
// last-resort fallback for kinds the core can't split (e.g. `internal` from a flat
// file/channel error string) or for non-CommandError rejections. Keep this the single
// place that knows the error shape.
export type SendFailReason =
  | "peer-unknown" // the recipient/peer isn't known / not discovered / offline
  | "relay-unreachable" // the node/relay/network couldn't carry the message
  | "crypto" // encryption / key / session failure
  | "unknown";

export function sendFailReason(e: unknown): SendFailReason {
  // 1) Map from the structured kind when the backend gave us one it can vouch for.
  switch (errorKind(e)) {
    case "peer-unknown":
      return "peer-unknown";
    case "relay-unreachable":
    case "network": // legacy
      return "relay-unreachable";
    case "crypto":
      return "crypto";
    // `io`, `not-started`, `auth`, `invalid-input`, `internal`, and the legacy
    // catch-all kinds carry no reliable send-cause on their own — fall through to
    // keyword-sniffing the message (covers `internal` file/channel error strings).
  }

  // 2) Fallback: sniff the message text.
  const msg = errorMessage(e).toLowerCase();
  if (/crypt|encrypt|decrypt|\bkey\b|ratchet|session|cipher/.test(msg))
    return "crypto";
  if (
    /unknown peer|no peer|peer not|not found|undiscovered|not discovered|no route|recipient|account not|offline/.test(
      msg,
    )
  )
    return "peer-unknown";
  if (
    /network|unreachable|connect|timed out|timeout|relay|post office|post-office|node down|not ready|not started|transport|send fail/.test(
      msg,
    )
  )
    return "relay-unreachable";
  return "unknown";
}
