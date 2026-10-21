import { useState } from "react";
import { ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import { useChat } from "@/store/chat";
import { usePrivacy } from "@/store/privacy";
import {
  postOfficeAvailability,
  selectablePostOffices,
  savedIndirectPostOffice,
} from "@/lib/postOfficeOptions";
import { PrivacySettings } from "../PrivacySettings";
import { OfflineConnectDialog } from "../OfflineConnectDialog";
import { SettingsRow, SELECT_CLASS, InactiveHelp } from "./primitives";
import { networkControlState } from "./model";
import type { SettingsController } from "./useSettingsController";

export function NetworkSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const [connectOpen, setConnectOpen] = useState(false);
  const peers = useChat((s) => s.peers);
  const policyOwner = usePrivacy((s) => s.owner);
  const loaded = usePrivacy((s) => s.loaded);
  const loading = usePrivacy((s) => s.loading);
  const busy = usePrivacy((s) => s.busy);
  const snapshot = usePrivacy((s) => s.snapshot);
  const s = c.settings;
  const invisible =
    policyOwner === c.owner && loaded && !loading && !busy && snapshot
      ? snapshot.invisible
      : null;
  const availability = networkControlState({
    loaded: !!s,
    saving: c.saving,
    relayEnabled: s?.post_office_enabled ?? false,
    physicalOnly: s?.physical_lan_only ?? false,
    invisible,
    send: s?.post_office_send_enabled ?? false,
    receive: s?.post_office_receive_enabled ?? false,
  });
  const relays = selectablePostOffices(peers);
  const indirect = savedIndirectPostOffice(peers, s?.bridge_hub_id ?? "");
  const icon = <ShieldCheck className="h-4 w-4" />;
  return (
    <>
      <PrivacySettings />
      {s && (
        <div className="mt-5">
          <SettingsRow
            id="setting-discovery-scope"
            icon={icon}
            title={t("settings.discoveryScope")}
            desc={t(
              s.physical_lan_only
                ? "settings.discoveryScopeLocalDesc"
                : "settings.discoveryScopeRoutedDesc",
            )}
            control={
              <select
                id="setting-discovery-scope"
                value={s.physical_lan_only ? "local" : "routed"}
                disabled={c.saving}
                onChange={(event) =>
                  void c.update(
                    { physical_lan_only: event.target.value === "local" },
                    "connection",
                  )
                }
                className={`${SELECT_CLASS} w-56`}
              >
                <option value="local">
                  {t("settings.discoveryScopeLocal")}
                </option>
                <option value="routed">
                  {t("settings.discoveryScopeRouted")}
                </option>
              </select>
            }
          />
          <div className="flex justify-end py-2 max-[700px]:justify-start">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => setConnectOpen(true)}
            >
              {t("settings.addDeviceAddress")}
            </Button>
          </div>
          <OfflineConnectDialog
            key={c.owner}
            open={connectOpen}
            onOpenChange={setConnectOpen}
            includeSharedNetwork
          />
          <SettingsRow
            id="setting-bridge-discovery"
            icon={icon}
            title={t("settings.bridgeDiscovery")}
            desc={t("settings.bridgeDiscoveryDesc")}
            reason={<InactiveHelp reason={availability.directory.reason} />}
            control={
              <Switch
                id="setting-bridge-discovery"
                checked={s.bridge_discovery_enabled}
                disabled={availability.directory.disabled}
                onCheckedChange={(on) =>
                  void c.update({ bridge_discovery_enabled: on }, "connection")
                }
              />
            }
          />
          <div className="pt-5">
            <SettingsRow
              id="setting-post-office"
              icon={icon}
              title={t("settings.postOffice")}
              desc={t("settings.postOfficeDesc")}
              control={
                <Switch
                  id="setting-post-office"
                  checked={s.post_office_enabled}
                  disabled={c.saving}
                  onCheckedChange={(on) =>
                    void c.update({ post_office_enabled: on }, "connection")
                  }
                />
              }
            />
            <p
              role="status"
              data-testid="post-office-status"
              className="mb-3 text-xs leading-relaxed text-muted-foreground"
            >
              {t(
                {
                  disabled: "settings.postOfficeStatusDisabled",
                  available: "settings.postOfficeStatusAvailable",
                  unavailable: "settings.postOfficeStatusUnavailable",
                }[postOfficeAvailability(s.post_office_enabled, peers)],
              )}
            </p>
            <div className="ml-2 border-l pl-4">
              {(
                [
                  {
                    id: "post-office-send",
                    key: "postOfficeSend",
                    field: "post_office_send_enabled",
                  },
                  {
                    id: "post-office-receive",
                    key: "postOfficeReceive",
                    field: "post_office_receive_enabled",
                  },
                ] as const
              ).map((option) => (
                <SettingsRow
                  key={option.id}
                  id={`setting-${option.id}`}
                  title={t(`settings.${option.key}`)}
                  desc={t(`settings.${option.key}Desc`)}
                  reason={<InactiveHelp reason={availability.relay.reason} />}
                  control={
                    <Switch
                      id={`setting-${option.id}`}
                      checked={s[option.field]}
                      disabled={availability.relay.disabled}
                      onCheckedChange={(on) =>
                        void c.update({ [option.field]: on }, "connection")
                      }
                    />
                  }
                />
              ))}
              <SettingsRow
                id="setting-bridge-hub"
                title={t("settings.bridgeHub")}
                desc={
                  <>
                    {t("settings.bridgeHubDesc")}
                    <p className="mt-1">{t("settings.relayPreferenceHint")}</p>
                  </>
                }
                reason={<InactiveHelp reason={availability.relay.reason} />}
                control={
                  <select
                    id="setting-bridge-hub"
                    value={s.bridge_hub_id}
                    disabled={availability.relay.disabled}
                    onChange={(e) =>
                      void c.update(
                        { bridge_hub_id: e.target.value },
                        "connection",
                      )
                    }
                    className={`${SELECT_CLASS} w-48`}
                  >
                    <option value="">{t("settings.bridgeHubAutomatic")}</option>
                    {s.bridge_hub_id &&
                      !relays.some(
                        (peer) => peer.user_id === s.bridge_hub_id,
                      ) && (
                        <option value={s.bridge_hub_id} disabled>
                          {indirect
                            ? t("settings.bridgeHubRemote", {
                                name: indirect.name,
                              })
                            : t("settings.bridgeHubUnavailable", {
                                id: s.bridge_hub_id.slice(0, 8),
                              })}
                        </option>
                      )}
                    {relays.map((peer) => (
                      <option key={peer.user_id} value={peer.user_id}>
                        {peer.name} · {peer.user_id.slice(0, 8)}
                      </option>
                    ))}
                  </select>
                }
              />
            </div>
            {availability.partialReceive && (
              <p
                role="status"
                className="mt-3 text-xs leading-relaxed text-muted-foreground"
              >
                {t("settings.partialReceiveWarning")}
              </p>
            )}
          </div>
        </div>
      )}
    </>
  );
}
