import type { MessageProtection } from "@/lib/types";
export interface DraftAdmissionResult {
  accepted: boolean;
  cleared: boolean;
}
interface Draft {
  protection?: MessageProtection;
  text: string;
  revision: number;
  pending: boolean;
}

/** Drafts and in-flight admission outlive keyed composers, scoped by owner/session/conversation. */
export class ConversationDrafts {
  private drafts = new Map<string, Draft>();

  get(key: string): string {
    return this.drafts.get(key)?.text ?? "";
  }

  getProtection(key: string): MessageProtection | undefined {
    return this.drafts.get(key)?.protection;
  }

  touch(key: string) {
    const draft = this.drafts.get(key);
    if (draft) draft.revision++;
  }

  captureProtection(key: string) {
    const draft = this.drafts.get(key);
    const revision = draft?.revision;
    return {
      protection: draft?.protection,
      accept: () => {
        if (
          !draft ||
          this.drafts.get(key) !== draft ||
          draft.revision !== revision
        )
          return false;
        this.setProtection(key, undefined);
        return true;
      },
    };
  }

  setProtection(key: string, protection: MessageProtection | undefined): void {
    const draft = this.drafts.get(key);
    if (!draft) {
      if (protection)
        this.drafts.set(key, {
          text: "",
          revision: 0,
          pending: false,
          protection,
        });
      return;
    }
    if (draft.protection === protection) return;
    draft.protection = protection;
    draft.revision++;
    if (!draft.text && !draft.pending && !protection) this.drafts.delete(key);
  }

  set(key: string, text: string): void {
    const draft = this.drafts.get(key);
    if (!draft) {
      if (text) this.drafts.set(key, { text, revision: 0, pending: false });
      return;
    }
    if (draft.text === text) return;
    draft.text = text;
    draft.revision += 1;
    if (!text && !draft.pending && !draft.protection) this.drafts.delete(key);
  }

  isPending(key: string): boolean {
    return this.drafts.get(key)?.pending ?? false;
  }

  admit(
    key: string,
    text: string,
    submit: () => Promise<boolean>,
  ): Promise<DraftAdmissionResult> | null {
    const draft = this.drafts.get(key);
    if (
      !draft ||
      draft.pending ||
      !text.trim() ||
      draft.text.trim() !== text.trim()
    )
      return null;
    const revision = draft.revision;
    draft.pending = true;
    const finish = (accepted: boolean): DraftAdmissionResult => {
      draft.pending = false;
      const cleared = accepted && draft.revision === revision;
      if (cleared) {
        draft.text = "";
        draft.protection = undefined;
        draft.revision += 1;
      }
      if (!draft.text && !draft.protection) this.drafts.delete(key);
      return { accepted, cleared };
    };
    try {
      return submit().then(finish, () => finish(false));
    } catch {
      return Promise.resolve(finish(false));
    }
  }
}
