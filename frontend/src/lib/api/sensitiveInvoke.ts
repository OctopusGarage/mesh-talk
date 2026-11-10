import { invoke } from "@tauri-apps/api/core";
import { errorKind } from "../error";

export interface SensitiveCredential {
  password: string;
  current: () => boolean;
}
type Verification = (command: string) => Promise<SensitiveCredential | null>;
let verification: Verification | null = null;

/** The application supplies UI; low-level adapters never import stores or views. */
export function registerSensitiveVerification(verify: Verification) {
  verification = verify;
  return () => {
    if (verification === verify) verification = null;
  };
}

export async function invokeSensitive<T>(
  command: string,
  args: Record<string, unknown>,
): Promise<T> {
  // The prompt may outlive edits to the caller's objects. Never authorize those edits.
  const captured = structuredClone(args);
  const verify = verification;
  try {
    return await invoke<T>(command, captured);
  } catch (error) {
    if (errorKind(error) !== "authentication-required" || !verify) throw error;
    if (verification !== verify)
      throw {
        kind: "authentication-canceled",
        message: "Verification canceled",
      };
    const credential = await verify(command);
    if (!credential || verification !== verify || !credential.current())
      throw {
        kind: "authentication-canceled",
        message: "Verification canceled",
      };
    // Do not loop on incorrect credentials, or retain a reusable unlocked state.
    return invoke<T>(command, { ...captured, password: credential.password });
  }
}
