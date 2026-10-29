/** MTCHAT1 contains display snapshots, not signatures from historical authors. */
export const FORWARDED_RECORD_MAX_BYTES = 64 * 1024 * 1024;
export interface ForwardedFile {
  name: string;
  mime: string;
  size: number;
  bytes: Uint8Array;
}
export interface ForwardedMessage {
  sender_name: string;
  wall_clock: number;
  text: string;
  file: ForwardedFile | null;
}
export interface ForwardedRecord {
  format: "MTCHAT1";
  title: string;
  created_at_ms: number;
  messages: ForwardedMessage[];
}
export function isForwardedRecord(name: string, mime: string): boolean {
  return (
    name.toLowerCase().endsWith(".mtchat") ||
    mime === "application/vnd.mesh-talk.chat+json"
  );
}
function invalid(): never {
  throw new Error("Invalid or unsupported forwarded chat record");
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return invalid();
  return value as Record<string, unknown>;
}
function text(value: unknown, limit: number): string {
  if (
    typeof value !== "string" ||
    new TextEncoder().encode(value).length > limit
  )
    return invalid();
  return value;
}
function time(value: unknown): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < 0 ||
    value > 8_640_000_000_000_000
  )
    return invalid();
  return value;
}
function file(value: unknown): ForwardedFile | null {
  if (value == null) return null;
  const source = object(value);
  const name = text(source.name, 255);
  const mime = text(source.mime, 255);
  if (
    !name ||
    /[/\\]/.test(name) ||
    [...name].some((char) => char.charCodeAt(0) < 32) ||
    name === "." ||
    name === ".." ||
    isForwardedRecord(name, mime)
  )
    return invalid();
  if (
    typeof source.size !== "number" ||
    !Number.isSafeInteger(source.size) ||
    source.size < 0 ||
    source.size > FORWARDED_RECORD_MAX_BYTES
  )
    return invalid();
  const encoded = text(source.data_base64, FORWARDED_RECORD_MAX_BYTES);
  if (
    encoded.length % 4 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
      encoded,
    )
  )
    return invalid();
  const binary = atob(encoded);
  if (binary.length !== source.size || btoa(binary) !== encoded)
    return invalid();
  return {
    name,
    mime,
    size: source.size,
    bytes: Uint8Array.from(binary, (c) => c.charCodeAt(0)),
  };
}
export function decodeForwardedRecord(
  input: ArrayBuffer | Uint8Array,
): ForwardedRecord {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  if (bytes.byteLength > FORWARDED_RECORD_MAX_BYTES) return invalid();
  const value = object(
    JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)),
  );
  if (
    value.format !== "MTCHAT1" ||
    !Array.isArray(value.messages) ||
    value.messages.length < 1 ||
    value.messages.length > 100
  )
    return invalid();
  return {
    format: "MTCHAT1",
    title: text(value.title, 512),
    created_at_ms: time(value.created_at_ms),
    messages: value.messages.map((raw) => {
      const item = object(raw);
      // New policy-bearing formats require an explicit version rather than silent downgrade.
      if (item.protection != null || item.expires_at_ms != null)
        return invalid();
      return {
        sender_name: text(item.sender_name, 256),
        wall_clock: time(item.wall_clock),
        text: text(item.text, 256 * 1024),
        file: file(item.file),
      };
    }),
  };
}
