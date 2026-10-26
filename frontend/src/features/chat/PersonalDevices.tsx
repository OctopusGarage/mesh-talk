import { useState } from "react";
import { ChevronDown, Monitor } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { chat, type DeviceSyncStatus } from "@/lib/api/chat";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { usePresenceFor } from "@/store/presence";
import { LinkDeviceDialog } from "./LinkDeviceDialog";
import { useSerialPoll } from "./useSerialPoll";

/** Retire both the disclosure and pending reads when its account/runtime changes. */
export function PersonalDevices() {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const accountId = useChat((s) => s.myAccountId);
  const deviceId = useChat((s) => s.myId);
  const runtime = useChat((s) => `${s.runEpoch}:${s.identityEpoch}`);
  if (!owner || !accountId || !deviceId) return null;
  return (
    <AccountDevices
      key={`${owner}:${generation}:${accountId}:${deviceId}:${runtime}`}
      owner={owner}
      accountId={accountId}
      deviceId={deviceId}
    />
  );
}

function AccountDevices({
  owner,
  accountId,
  deviceId,
}: {
  owner: string;
  accountId: string;
  deviceId: string;
}) {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(accountId);
  const [statuses, setStatuses] = useState<DeviceSyncStatus[] | null>(null);
  const [hostnames, setHostnames] = useState<Record<string, string>>({});
  useSerialPoll(
    true,
    5_000,
    () => chat.getDeviceHostnames(owner, accountId),
    setHostnames,
    () => setHostnames({}),
    "state",
  );
  const [failed, setFailed] = useState(false);
  useSerialPoll(
    true,
    5_000,
    () => chat.getDeviceSyncStatus(owner),
    (next) => {
      setStatuses(next);
      setFailed(false);
    },
    () => setFailed(true),
    "state",
  );
  // Inventory comes from the persistent certified roster, never discovery alone.
  // Sync phase describes replication, not whether a device is currently online.
  const online = new Set(
    presence.devices
      ?.filter((device) => device.evidence.some((e) => e.expires_in_secs > 0))
      .map((device) => device.user_id),
  );
  const siblings = (statuses ?? [])
    .filter((s) => s.device_id !== deviceId)
    .sort(
      (a, b) =>
        Number(online.has(b.device_id)) - Number(online.has(a.device_id)) ||
        a.device_id.localeCompare(b.device_id),
    );
  const onlineCount =
    1 + siblings.filter((s) => online.has(s.device_id)).length;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="shrink-0 gap-1.5 text-xs text-muted-foreground"
          data-testid="personal-devices-trigger"
        >
          <Monitor className="h-3.5 w-3.5" aria-hidden="true" />
          {t("personal.devices")}
          {statuses !== null && !failed && (
            <span className="tabular-nums">
              · {t("personal.onlineCount", { count: onlineCount })}
            </span>
          )}
          <ChevronDown className="h-3 w-3" aria-hidden="true" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-80 max-w-[calc(100vw-2rem)] p-3"
        data-testid="personal-devices"
        aria-label={t("personal.devices")}
      >
        <h2 className="font-display text-sm font-semibold">
          {t("personal.devices")}
        </h2>
        <p className="mt-1 text-xs text-muted-foreground">
          {t("personal.devicesDescription")}
        </p>
        <ul
          className="mt-3 max-h-64 space-y-3 overflow-y-auto"
          aria-label={t("personal.devices")}
        >
          <li data-device-id={deviceId}>
            <div className="break-all text-sm font-medium">
              {hostnames[deviceId]
                ? `${hostnames[deviceId]} · ${t("personal.thisDevice")}`
                : t("personal.thisDevice")}
            </div>
            <div className="select-text break-all font-mono text-xs text-muted-foreground">
              {deviceId}
            </div>
          </li>
          {siblings.map((device) => {
            const peer = peers.find(
              (p) =>
                p.user_id === device.device_id &&
                p.account_id === accountId &&
                !p.post_office,
            );
            return (
              <li key={device.device_id} data-device-id={device.device_id}>
                <div className="flex items-start justify-between gap-3 text-sm">
                  <span className="min-w-0 break-all font-medium">
                    {hostnames[device.device_id] ||
                      peer?.name ||
                      t("personal.linkedDevice")}
                  </span>
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {t(
                      online.has(device.device_id)
                        ? "presence.online"
                        : "presence.offline",
                    )}
                  </span>
                </div>
                <div className="select-text break-all font-mono text-xs text-muted-foreground">
                  {device.device_id}
                </div>
              </li>
            );
          })}
        </ul>
        {failed ? (
          <p role="status" className="mt-3 text-xs text-destructive">
            {t("personal.devicesFailed")}
          </p>
        ) : statuses === null ? (
          <p role="status" className="mt-3 text-xs text-muted-foreground">
            {t("personal.devicesLoading")}
          </p>
        ) : siblings.length === 0 ? (
          <p className="mt-3 text-xs text-muted-foreground">
            {t("personal.noOtherDevices")}
          </p>
        ) : null}
        <div className="mt-3 border-t pt-2">
          <LinkDeviceDialog menuItem />
        </div>
      </PopoverContent>
    </Popover>
  );
}
