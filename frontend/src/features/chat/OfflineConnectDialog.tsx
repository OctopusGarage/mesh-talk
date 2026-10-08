import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Cable, Radar, Wifi } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useMotionOK } from "@/lib/motion";
import { diag } from "@/lib/api";
import { errorMessage } from "@/lib/error";

/** The signature mark: two devices joined by a signal arc with nothing between them —
 * mesh-talk needs no router or tower, just a direct link. The arc traces itself once on a
 * calm machine; static when reduced-motion is asked for. */
function DirectLinkGlyph() {
  const motionOK = useMotionOK();
  return (
    <div
      aria-hidden
      className="flex justify-center py-1"
      data-testid="offline-connect-glyph"
    >
      <svg width="208" height="78" viewBox="0 0 208 78" fill="none">
        {/* the direct arc, signal-teal */}
        <path
          d="M44 50 C 84 6, 124 6, 164 50"
          className="stroke-signal"
          strokeWidth="2"
          strokeLinecap="round"
          strokeDasharray={motionOK ? "4 6" : undefined}
          opacity="0.85"
        >
          {motionOK && (
            <animate
              attributeName="stroke-dashoffset"
              from="40"
              to="0"
              dur="1.1s"
              fill="freeze"
            />
          )}
        </path>
        {/* a single hop pulse riding the arc */}
        {motionOK && (
          <circle r="3" className="fill-signal">
            <animateMotion
              path="M44 50 C 84 6, 124 6, 164 50"
              dur="2.4s"
              repeatCount="indefinite"
              keyPoints="0;1"
              keyTimes="0;1"
              calcMode="linear"
            />
          </circle>
        )}
        {/* two devices */}
        {[28, 148].map((x) => (
          <g key={x}>
            <rect
              x={x}
              y="44"
              width="32"
              height="22"
              rx="4"
              className="fill-card stroke-border"
              strokeWidth="1.5"
            />
            <rect
              x={x + 6}
              y="49"
              width="20"
              height="2"
              rx="1"
              className="fill-muted-foreground/50"
            />
          </g>
        ))}
      </svg>
    </div>
  );
}

/** A boxed step, numbered because the order matters: you host first, then join. */
function Step({
  n,
  icon,
  title,
  children,
}: {
  n: number;
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-lg border bg-card/40 p-3">
      <p className="flex items-center gap-2 font-display text-sm font-semibold tracking-tight">
        <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-signal/15 font-mono text-xs text-signal">
          {n}
        </span>
        <span className="text-signal">{icon}</span>
        {title}
      </p>
      <div className="mt-2 space-y-1 pl-7 text-sm text-muted-foreground">
        {children}
      </div>
    </section>
  );
}

/** A guide that surfaces when a peer can't find anyone: how to make a direct link over a
 * phone-style hotspot when there's no shared Wi-Fi. Controlled — the caller owns `open`. */
export function OfflineConnectDialog({
  open,
  onOpenChange,
  noNetwork = false,
  includeSharedNetwork = false,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  noNetwork?: boolean;
  includeSharedNetwork?: boolean;
}) {
  const { t } = useTranslation();
  const [announcing, setAnnouncing] = useState(false);
  const [announced, setAnnounced] = useState(false);
  const [announceError, setAnnounceError] = useState<string | null>(null);
  const [showHotspot, setShowHotspot] = useState(false);
  const announceRequest = useRef(0);
  const announce = async () => {
    const request = ++announceRequest.current;
    setAnnouncing(true);
    setAnnounced(false);
    setAnnounceError(null);
    try {
      await diag.rescan();
      if (request === announceRequest.current) setAnnounced(true);
    } catch (error) {
      if (request === announceRequest.current)
        setAnnounceError(errorMessage(error));
    } finally {
      if (request === announceRequest.current) setAnnouncing(false);
    }
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) {
          announceRequest.current++;
          setAnnouncing(false);
          setAnnounced(false);
          setAnnounceError(null);
          setShowHotspot(false);
        }
      }}
    >
      <DialogContent className="max-w-lg" data-testid="offline-connect-dialog">
        <DialogHeader>
          <DialogTitle>
            {t(
              includeSharedNetwork
                ? "redesign.connectToSomeone"
                : "offlineConnect.title",
            )}
          </DialogTitle>
          <DialogDescription>
            {includeSharedNetwork
              ? t("redesign.connectIntro")
              : noNetwork
                ? t("offlineConnect.introNoNetwork")
                : t("offlineConnect.intro")}
          </DialogDescription>
        </DialogHeader>

        {!includeSharedNetwork && <DirectLinkGlyph />}

        <div className="space-y-3">
          {includeSharedNetwork && (
            <section className="space-y-2 rounded-lg border p-3">
              <p className="flex items-center gap-2 text-sm font-semibold">
                <Wifi className="h-4 w-4 text-signal" aria-hidden="true" />
                {t("redesign.sameNetworkTitle")}
              </p>
              <p className="text-sm leading-relaxed text-muted-foreground">
                {t("redesign.sameNetworkBody")}
              </p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                data-testid="connect-announce"
                disabled={announcing}
                onClick={() => void announce()}
              >
                {announcing
                  ? t("common.loading")
                  : announced
                    ? t("diagnostics.rescanned")
                    : t("diagnostics.rescan")}
              </Button>
              {announced && (
                <p role="status" className="text-xs text-muted-foreground">
                  {t("redesign.announceFeedback")}
                </p>
              )}
              {announceError && (
                <p
                  role="alert"
                  className="break-words text-xs text-destructive"
                >
                  {t("diagnostics.actionFailed", { error: announceError })}
                </p>
              )}
            </section>
          )}
          {includeSharedNetwork && (
            <div className="flex flex-wrap items-center justify-between gap-2 px-1">
              <p className="text-xs text-muted-foreground">
                {t("redesign.noSharedNetwork")}
              </p>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                aria-expanded={showHotspot}
                onClick={() => setShowHotspot((visible) => !visible)}
              >
                {t("offlineConnect.trigger")}
              </Button>
            </div>
          )}
          {(!includeSharedNetwork || showHotspot) && (
            <div className="space-y-3">
              <Step
                n={1}
                icon={<Wifi className="h-4 w-4" />}
                title={t("offlineConnect.hostTitle")}
              >
                <p>{t("offlineConnect.hostMac")}</p>
                <p>{t("offlineConnect.hostWin")}</p>
                <p>{t("offlineConnect.hostLinux")}</p>
              </Step>
              <Step
                n={2}
                icon={<Radar className="h-4 w-4" />}
                title={t("offlineConnect.joinTitle")}
              >
                <p>{t("offlineConnect.joinBody")}</p>
              </Step>
              <p className="flex items-start gap-2 px-1 text-xs leading-relaxed text-muted-foreground">
                <Cable className="mt-0.5 h-3.5 w-3.5 shrink-0 text-signal" />
                {t("offlineConnect.wiredNote")}
              </p>
            </div>
          )}
        </div>

        <div className="flex justify-end">
          <Button onClick={() => onOpenChange(false)}>
            {t("offlineConnect.gotIt")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
