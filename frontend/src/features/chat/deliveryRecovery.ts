import { createContext, useContext } from "react";

export const DeliveryRecoveryContext = createContext<
  | ((target: "settings" | "diagnostics", origin: HTMLElement | null) => void)
  | null
>(null);

export function useDeliveryRecovery() {
  return useContext(DeliveryRecoveryContext);
}
