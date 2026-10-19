/** The approval continuation owns ordering and stale-session checks, not view state. */
export function createAttachmentDecision(context: {
  captureOwnership: () => { current(): boolean };
  decideAttachment: (fileConv: string, accept: boolean) => Promise<void>;
  refreshAvailability: (fileConvs: string[]) => Promise<unknown>;
  reportError: (error: unknown) => void;
}) {
  return async (fileConv: string, accept: boolean): Promise<void> => {
    const lease = context.captureOwnership();
    if (!lease.current() || !fileConv) return;
    try {
      await context.decideAttachment(fileConv, accept);
      if (lease.current()) await context.refreshAvailability([fileConv]);
    } catch (error) {
      if (lease.current()) context.reportError(error);
    }
  };
}
