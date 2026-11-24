import { Phone, PhoneIncoming } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  joinGroupVoice,
  muteGroupVoice,
  startGroupVoice,
  stopGroupVoice,
  useGroupVoice,
} from "@/store/groupVoice";
import { useCalls } from "@/store/calls";
import { useSettings } from "@/store/settings";

export function GroupVoiceEntry({
  group,
  self,
  owner,
  memberDevices,
}: {
  group: string;
  self: string;
  owner: string;
  memberDevices: () => string[];
}) {
  const { t } = useTranslation();
  const enabled = useSettings((state) => state.callsEnabled);
  const invite = useGroupVoice((state) => state.invites[group]);
  const active = useGroupVoice((state) => state.active);
  const directCall = useCalls((state) => state.phase !== "idle");
  if (!enabled || active || directCall) return null;
  const canJoin = invite && invite.expires > Date.now();
  const label = t(canJoin ? "groupVoice.join" : "groupVoice.start");
  const Icon = canJoin ? PhoneIncoming : Phone;
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon"
      aria-label={label}
      title={label}
      onClick={() => {
        if (invite && invite.expires > Date.now())
          void joinGroupVoice(invite, self, owner);
        else void startGroupVoice(group, self, owner, memberDevices());
      }}
    >
      <Icon aria-hidden="true" className="h-4 w-4" />
    </Button>
  );
}

function RemoteAudio({ stream }: { stream: MediaStream }) {
  const { t } = useTranslation();
  const element = useRef<HTMLAudioElement>(null);
  const [blocked, setBlocked] = useState(false);
  useEffect(() => {
    const player = element.current;
    if (!player) return;
    let live = true;
    player.srcObject = stream;
    void player.play().then(
      () => {
        if (live) setBlocked(false);
      },
      () => {
        if (live) setBlocked(true);
      },
    );
    return () => {
      live = false;
      player.pause();
      player.srcObject = null;
    };
  }, [stream]);
  return (
    <>
      <audio ref={element} autoPlay />
      {blocked && (
        <Button
          onClick={() => {
            void element.current?.play().then(
              () => setBlocked(false),
              () => setBlocked(true),
            );
          }}
        >
          {t("groupVoice.play")}
        </Button>
      )}
    </>
  );
}

export function GroupVoiceControls() {
  const { t } = useTranslation();
  const { active, audio, muted, error } = useGroupVoice();
  if (!active && !error) return null;
  return (
    <section
      aria-label={t("groupVoice.title")}
      className="absolute bottom-6 right-6 z-40 rounded-lg border bg-popover p-4 text-popover-foreground shadow-elevation"
    >
      {active && (
        <div className="flex items-center gap-2">
          <span role="status">
            {active.joining ? t("groupVoice.joining") : t("groupVoice.title")}
          </span>
          <Button onClick={muteGroupVoice} aria-pressed={muted}>
            {muted ? t("groupVoice.unmute") : t("groupVoice.mute")}
          </Button>
          <Button onClick={() => stopGroupVoice(true)}>
            {t("groupVoice.leave")}
          </Button>
          {active.host && (
            <Button
              variant="destructive"
              onClick={() => stopGroupVoice(true, true)}
            >
              {t("groupVoice.end")}
            </Button>
          )}
        </div>
      )}
      {error && (
        <div className="flex items-center gap-2">
          <span role="alert">{t(error)}</span>
          <Button onClick={() => useGroupVoice.setState({ error: null })}>
            {t("groupVoice.dismiss")}
          </Button>
        </div>
      )}
      {Object.entries(audio).map(([device, stream]) => (
        <RemoteAudio key={device} stream={stream} />
      ))}
    </section>
  );
}
