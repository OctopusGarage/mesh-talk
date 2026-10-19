import { errorMessage } from "@/lib/error";

/** Progress records store handoff, which does not claim backend acceptance. */
export class AttachmentBatchError extends Error {
  constructor(
    readonly cause: unknown,
    readonly handedOffCount: number,
  ) {
    super(errorMessage(cause));
    this.name = "AttachmentBatchError";
  }
}

/** Remove submitted occurrences while preserving files pasted during an in-flight send. */
export function removeHandedOffFiles(
  pending: File[],
  submitted: File[],
  count: number,
): File[] {
  const handedOff = submitted.slice(0, count);
  return pending.filter((file) => {
    const index = handedOff.indexOf(file);
    if (index < 0) return true;
    handedOff.splice(index, 1);
    return false;
  });
}
