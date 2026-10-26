import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { chat as chatApi } from "@/lib/api";
import type { ChannelMemberInfo } from "@/lib/types";
import { useAuth } from "@/store/auth";
import { captureRuntimeOwner } from "@/store/ownership";
import { ChannelHistorySettings } from "./ChannelHistorySettings";

export interface GroupPreferencesProps {
  channelId: string;
  myId: string;
  channelOwner: string;
  dissolved: boolean;
  sharedReference: boolean;
  referenceOnly: boolean;
  canManage: boolean;
  selfAccepted: boolean;
  ownMember?: ChannelMemberInfo;
}

export function GroupPreferences({
  channelId,
  myId,
  channelOwner,
  dissolved,
  sharedReference,
  referenceOnly,
  canManage,
  selfAccepted,
  ownMember,
}: GroupPreferencesProps) {
  const { t } = useTranslation();
  const localOwner = useAuth((state) => state.user?.id);
  const [replicaOverride, setReplicaOverride] = useState<boolean | null>(null);
  const [fileOverride, setFileOverride] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [lifetime] = useState(() => ({ live: true }));
  const context = useRef({ channelId, myId });
  context.current = { channelId, myId };
  useEffect(() => {
    lifetime.live = true;
    return () => {
      lifetime.live = false;
    };
  }, [lifetime]);
  useEffect(() => {
    setReplicaOverride(null);
  }, [ownMember?.replica_enabled]);
  useEffect(() => {
    setFileOverride(null);
  }, [
    ownMember?.file_sharing?.network_cost,
    ownMember?.file_sharing?.override_enabled,
  ]);

  const replicaEnabled =
    (replicaOverride ?? ownMember?.replica_enabled) !== false;
  const fileSharing = ownMember?.file_sharing;
  const fileOverrideEnabled =
    fileOverride ?? fileSharing?.override_enabled ?? false;
  const localSharing =
    !sharedReference && !dissolved && ownMember?.user_id === myId;

  const currentOperation = () => {
    const lease = captureRuntimeOwner();
    return {
      owner: lease.owner,
      current: () =>
        lifetime.live &&
        lease.current() &&
        context.current.channelId === channelId &&
        context.current.myId === myId,
    };
  };

  const setReplica = async () => {
    if (!localSharing || busy || ownMember?.pending) return;
    const operation = currentOperation();
    if (!operation.owner || !operation.current()) return;
    const enabled = !replicaEnabled;
    setBusy(true);
    setError(false);
    try {
      await chatApi.setChannelReplicaEnabled(channelId, enabled);
      if (operation.current()) setReplicaOverride(enabled);
    } catch {
      if (operation.current()) setError(true);
    } finally {
      if (operation.current()) setBusy(false);
    }
  };

  const setFileSharing = async () => {
    if (!localSharing || busy || ownMember?.pending || !fileSharing) return;
    const operation = currentOperation();
    if (!operation.owner || !operation.current()) return;
    const enabled = !fileOverrideEnabled;
    setBusy(true);
    setError(false);
    try {
      await chatApi.setGroupFileSharingOverride(
        operation.owner,
        fileSharing.network_cost,
        enabled,
      );
      if (operation.current()) setFileOverride(enabled);
    } catch {
      if (operation.current()) setError(true);
    } finally {
      if (operation.current()) setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      {localSharing && (
        <section
          className="space-y-2"
          aria-label={t("members.yourPreferences")}
        >
          <h3 className="text-sm font-medium">
            {t("members.yourPreferences")}
          </h3>
          <p className="text-xs text-muted-foreground">
            {t("members.replicaHelp")}
          </p>
          {!replicaEnabled && (
            <p className="text-xs text-muted-foreground">
              {t("members.replicaDisabled")}
            </p>
          )}
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || ownMember.pending || !localOwner}
            onClick={() => void setReplica()}
          >
            {t(
              replicaEnabled
                ? "members.disableReplica"
                : "members.enableReplica",
            )}
          </Button>
          {replicaEnabled &&
            fileSharing &&
            fileSharing.network_cost !== "unrestricted" && (
              <div className="space-y-1 border-t border-border pt-3 text-xs text-muted-foreground">
                <p role="status">
                  {t(
                    fileOverrideEnabled
                      ? "members.fileSharingAllowed"
                      : fileSharing.network_cost === "unknown"
                        ? "members.fileSharingUnknown"
                        : "members.fileSharingMetered",
                  )}
                </p>
                <p>{t("members.fileSharingHelp")}</p>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy || ownMember.pending || !localOwner}
                  onClick={() => void setFileSharing()}
                >
                  {t(
                    fileOverrideEnabled
                      ? "members.fileSharingProtect"
                      : "members.fileSharingAllow",
                  )}
                </Button>
              </div>
            )}
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {t("members.replicaError")}
            </p>
          )}
        </section>
      )}
      {!referenceOnly && (
        <ChannelHistorySettings
          channelId={channelId}
          myId={myId}
          channelOwner={channelOwner}
          dissolved={dissolved}
          canManage={canManage}
          selfAccepted={selfAccepted}
        />
      )}
    </div>
  );
}
