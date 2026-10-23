import { AccountProfilePanel } from "./AccountProfilePanel";
import { displayIdentity } from "@/lib/displayIdentity";
import { MessageCircle } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { IdentityCrest } from "@/components/identity";
import { useChat } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { MemberProfileTarget } from "./memberProfileDetails";
import {
  memberDiscoverySources,
  memberProfileDevices,
} from "./memberProfileDetails";

export function MemberProfileDialog({
  target,
  onOpenChange,
  onMessage,
}: {
  target: MemberProfileTarget | null;
  onOpenChange: (open: boolean) => void;
  onMessage?: (accountId: string, name: string) => void;
}) {
  if (!target) return null;
  return (
    <MemberProfileContent
      target={target}
      onOpenChange={onOpenChange}
      onMessage={onMessage}
    />
  );
}

function MemberProfileContent({
  target,
  onOpenChange,
  onMessage,
}: {
  target: MemberProfileTarget;
  onOpenChange: (open: boolean) => void;
  onMessage?: (accountId: string, name: string) => void;
}) {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const accounts = useChat((s) => s.accounts);
  const favorites = useChat((s) => s.favorites);
  const members = useChat((s) => s.members);
  const currentMember = members?.find((member) =>
    target.deviceId
      ? member.user_id === target.deviceId
      : !!target.accountId && member.account_id === target.accountId,
  );
  const identity = displayIdentity({
    accountId: target.accountId || currentMember?.account_id,
    deviceId: target.deviceId,
    fallbackName:
      (target.isSelf ? null : currentMember?.name) ||
      target.canonicalName ||
      target.name,
    accounts,
    peers,
    favorites,
  });
  const presence = usePresenceFor(
    identity.accountId ||
      (target.deviceId ? `device:${target.deviceId}` : undefined),
  );
  const devices = memberProfileDevices(
    { ...target, accountId: identity.accountId },
    peers,
  );
  const discoverySources = memberDiscoverySources(presence);
  const name = identity.name;
  const accountId = identity.accountId;

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent
        className="max-h-[85dvh] max-w-sm overflow-y-auto"
        data-testid="member-profile-dialog"
      >
        <DialogHeader>
          <DialogTitle>{t("memberProfile.title")}</DialogTitle>
          <DialogDescription>
            {t("memberProfile.description")}
          </DialogDescription>
        </DialogHeader>
        <div className="rounded-xl border bg-muted/30 p-4">
          <IdentityCrest
            id={identity.id}
            name={name}
            hideId
            wrapName
            status={presenceStatus(presence)}
            variant="large"
          />
          <p className="mt-2 pl-[68px] text-xs text-muted-foreground">
            {presenceLabel(presence, t)}
          </p>
        </div>

        {accountId && (
          <AccountProfilePanel key={accountId} accountId={accountId} />
        )}

        <section
          aria-label={t("memberProfile.discoveredVia")}
          data-testid="member-discovery-sources"
          className="min-w-0 space-y-2"
        >
          <h3 className="text-sm font-medium">
            {t("memberProfile.discoveredVia")}
          </h3>
          {discoverySources.length ? (
            <ul className="space-y-2 text-sm">
              {discoverySources.map((entry) => (
                <li
                  key={JSON.stringify(entry)}
                  className="min-w-0 [overflow-wrap:anywhere]"
                >
                  {entry.source === "local"
                    ? t("memberProfile.discoveryLocal")
                    : entry.source === "directory"
                      ? entry.postOffice?.name
                        ? t("memberProfile.discoveryPostOffice", {
                            name: entry.postOffice.name,
                          })
                        : t("memberProfile.discoveryUnnamedOffice")
                      : t(`presence.source_${entry.source}`)}
                  {entry.postOffice && (
                    <code className="block select-text text-xs text-muted-foreground [overflow-wrap:anywhere]">
                      {entry.postOffice.endpoint}
                    </code>
                  )}
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-muted-foreground">
              {t("memberProfile.discoveryUnavailable")}
            </p>
          )}
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("memberProfile.discoveryHint")}
          </p>
        </section>

        <p className="text-xs text-muted-foreground">
          {t("presence.offlineHint")}
        </p>
        <p className="text-xs text-muted-foreground">
          {t("presence.communicationHint")}
        </p>
        {!!presence.devices?.length && (
          <div
            className="space-y-2 text-xs"
            data-testid="presence-device-details"
          >
            <p>
              {t("presence.observedDevices", {
                count: presence.devices.length,
              })}
            </p>
            {presence.devices.map((device) => (
              <div key={device.user_id}>
                <code className="block break-all">{device.user_id}</code>
                <span className="block">
                  {[...new Set(device.evidence.map((e) => e.source))]
                    .map((source) =>
                      t("presence.sourceAge", {
                        source: t(`presence.source_${source}`),
                        seconds: Math.min(
                          ...device.evidence
                            .filter((e) => e.source === source)
                            .map((e) => e.last_seen_secs),
                        ),
                      }),
                    )
                    .join(" · ")}
                </span>
              </div>
            ))}
          </div>
        )}
        <div className="space-y-3">
          {identity.alias && identity.alias !== identity.canonicalName && (
            <dl className="space-y-2 text-sm">
              <div>
                <dt className="text-xs text-muted-foreground">
                  {t("memberProfile.originalName")}
                </dt>
                <dd
                  data-testid="member-profile-original-name"
                  className="break-words"
                >
                  {identity.canonicalName}
                </dd>
              </div>
              <div>
                <dt className="text-xs text-muted-foreground">
                  {t("memberProfile.remark")}
                </dt>
                <dd data-testid="member-profile-remark" className="break-words">
                  {identity.alias}
                </dd>
              </div>
            </dl>
          )}
          <div>
            <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
              {t("memberProfile.devices")}
            </div>
            {devices.length === 0 ? (
              <p className="mt-1 text-sm text-muted-foreground">
                {t("memberProfile.noDevice")}
              </p>
            ) : (
              <div className="mt-1 space-y-2">
                {devices.map((device) => (
                  <div
                    key={device.id}
                    className="min-w-0 rounded-lg border px-3 py-2.5"
                  >
                    <div className="flex flex-wrap items-baseline justify-between gap-1">
                      <span className="text-xs font-medium">
                        {t("memberProfile.deviceId")}
                      </span>
                      {device.postOffice && (
                        <span className="text-[11px] text-muted-foreground">
                          {t("memberProfile.postOffice")}
                        </span>
                      )}
                    </div>
                    <code className="mt-1 block select-text break-all text-xs">
                      {device.id}
                    </code>
                    <div className="mt-2 text-[11px] font-medium text-muted-foreground">
                      {t("memberProfile.networkEndpoint")}
                    </div>
                    {device.endpoint ? (
                      <code className="block select-text text-xs [overflow-wrap:anywhere]">
                        {device.endpoint}
                      </code>
                    ) : (
                      <span className="block text-xs text-muted-foreground">
                        {t("memberProfile.unavailable")}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
        {devices.some((device) => device.endpoint) && (
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("memberProfile.endpointHint")}
          </p>
        )}
        {accountId && onMessage && !target.isSelf && (
          <Button
            data-testid="member-profile-message"
            className="w-full gap-2"
            onClick={() => {
              onOpenChange(false);
              onMessage(accountId, name);
            }}
          >
            <MessageCircle className="h-4 w-4" />
            {t("members.openDm")}
          </Button>
        )}
      </DialogContent>
    </Dialog>
  );
}
