/** Keep the wire parser authoritative; cap pasted artifacts before crossing IPC. */
export const MAX_TRUST_ARTIFACT_BYTES = 1_048_576;
export function checkedTrustArtifact(text: string): string {
  const json = text.trim();
  if (new TextEncoder().encode(json).length > MAX_TRUST_ARTIFACT_BYTES)
    throw new Error("Trust artifact is too large (maximum 1 MiB).");
  const value: unknown = JSON.parse(json);
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw new Error("Paste a signed card or proof JSON object.");
  return json;
}
export function creationDateMs(value: string): number | null {
  if (!value) return null;
  const ms = Date.parse(`${value}T00:00:00.000Z`);
  if (!Number.isFinite(ms) || new Date(ms).toISOString().slice(0, 10) !== value)
    throw new Error("Choose a valid creation date.");
  return ms;
}

/** Dates are signed data; u64 timestamps can exceed JavaScript's supported range. */
export function trustDateInput(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "";
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toISOString().slice(0, 10) : "";
}
export function trustDateLabel(value: number | null): string {
  if (value === null) return "unknown";
  const date = new Date(value);
  return Number.isFinite(date.getTime())
    ? date.toLocaleDateString(undefined, { timeZone: "UTC" })
    : "outside the supported date range";
}
