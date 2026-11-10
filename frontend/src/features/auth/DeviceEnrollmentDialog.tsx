import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, Smartphone } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { auth, type EnrollmentSourceInfo } from "@/lib/api/auth";
import { errorKind, errorMessage } from "@/lib/error";
import { shortId } from "@/lib/format";
import { useAuth } from "@/store/auth";

interface EnrollmentAttempt {
  id: string | null;
  active: boolean;
  scanning: boolean;
  completing: boolean;
  published: boolean;
}

/** Enroll a new device without first creating an unrelated local account. */
export function DeviceEnrollmentDialog() {
  const { t } = useTranslation();
  const { loading } = useAuth();
  const translate = useRef(t);
  useEffect(() => {
    translate.current = t;
  }, [t]);
  const [open, setOpen] = useState(false);
  const [restart, setRestart] = useState(0);
  const [phase, setPhase] = useState<
    "starting" | "scanning" | "idle" | "linking"
  >("starting");
  const [sources, setSources] = useState<EnrollmentSourceInfo[]>([]);
  const [sourceId, setSourceId] = useState("");
  const [targets, setTargets] = useState("");
  const [code, setCode] = useState("");
  const [consent, setConsent] = useState(false);
  const [includeHistory, setIncludeHistory] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const attempt = useRef<EnrollmentAttempt | null>(null);
  const cleanup = useRef<Promise<unknown>>(Promise.resolve());
  const current = (flow: EnrollmentAttempt) =>
    flow.active && attempt.current === flow;
  const failure = useCallback(
    (cause: unknown) =>
      errorKind(cause) === "link-upgrade-required"
        ? translate.current("linkDevice.credentialsUpgrade")
        : errorMessage(cause),
    [],
  );

  const probe = useCallback(
    async (flow: EnrollmentAttempt, addresses: string[]) => {
      if (!current(flow) || !flow.id || flow.scanning || flow.completing)
        return;
      flow.scanning = true;
      setPhase("scanning");
      setError(null);
      setSources([]);
      setSourceId("");
      setConsent(false);
      try {
        const found = await auth.probeDeviceEnrollment(flow.id, addresses);
        if (current(flow)) setSources(found);
      } catch (cause) {
        if (current(flow)) setError(failure(cause));
      } finally {
        flow.scanning = false;
        if (current(flow)) setPhase("idle");
      }
    },
    [failure],
  );

  useEffect(() => {
    if (!open) return;
    const flow: EnrollmentAttempt = {
      id: null,
      active: true,
      scanning: false,
      completing: false,
      published: false,
    };
    attempt.current = flow;
    setPhase("starting");
    setSources([]);
    setSourceId("");
    setConsent(false);
    setCode("");
    setError(null);
    setIncludeHistory(true);
    // Reopening waits for the prior attempt's late begin/cancellation, so it
    // cannot borrow a pending provisional node or cancel a replacement attempt.
    const started = cleanup.current.then(() =>
      current(flow) ? auth.beginDeviceEnrollment() : null,
    );
    void started
      .then(async (ticket) => {
        if (!ticket || !current(flow)) return;
        flow.id = ticket.id;
        await probe(flow, []);
      })
      .catch((cause) => {
        if (current(flow)) {
          setPhase("idle");
          setError(failure(cause));
        }
      });
    return () => {
      flow.active = false;
      cleanup.current = started
        .then((ticket) => {
          if (ticket && !flow.published)
            return auth.cancelDeviceEnrollment(ticket.id);
        })
        .catch(() => {});
    };
  }, [open, restart, probe, failure]);

  const selected = sources.find((source) => source.id === sourceId);
  const busy = phase !== "idle";
  const publishing = phase === "linking";
  const normalizedCode = code.replace(/\s+/g, "");
  const link = async () => {
    const flow = attempt.current;
    if (
      !flow ||
      !current(flow) ||
      !flow.id ||
      flow.completing ||
      flow.scanning ||
      !selected ||
      !consent ||
      !/^[a-f0-9]{32}$/i.test(normalizedCode)
    )
      return;
    flow.completing = true;
    setPhase("linking");
    setError(null);
    try {
      const accepted = await useAuth
        .getState()
        .enrollDevice(flow.id, selected.id, normalizedCode, includeHistory);
      if (accepted) {
        flow.published = true;
        if (current(flow)) setOpen(false);
      } else if (current(flow))
        setError(
          useAuth.getState().error ||
            translate.current("deviceEnrollment.failed"),
        );
    } finally {
      flow.completing = false;
      if (current(flow)) setPhase("idle");
    }
  };
  const scan = () => {
    const flow = attempt.current;
    if (!flow || !current(flow) || busy) return;
    if (!flow.id) setRestart((value) => value + 1);
    else void probe(flow, targets.split(/[\s,]+/).filter(Boolean));
  };
  const changeOpen = (next: boolean) => {
    if (!next && attempt.current?.completing) return;
    setOpen(next);
  };

  return (
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogTrigger asChild>
        <Button
          variant="outline"
          className="w-full"
          disabled={loading}
          data-testid="login-link-device"
        >
          <Smartphone className="h-4 w-4" />
          {t("deviceEnrollment.trigger")}
        </Button>
      </DialogTrigger>
      <DialogContent
        data-testid="device-enrollment-dialog"
        className={publishing ? "[&>button:last-child]:hidden" : undefined}
        onEscapeKeyDown={(event) => {
          if (attempt.current?.completing) event.preventDefault();
        }}
        onInteractOutside={(event) => {
          if (attempt.current?.completing) event.preventDefault();
        }}
      >
        <DialogHeader>
          <DialogTitle>{t("deviceEnrollment.title")}</DialogTitle>
          <DialogDescription>
            {t("deviceEnrollment.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="text-sm leading-relaxed">
          {t("deviceEnrollment.sourceInstructions")}
        </p>
        <div className="space-y-2">
          <Label htmlFor="enrollment-source">
            {t("deviceEnrollment.source")}
          </Label>
          <select
            id="enrollment-source"
            value={sourceId}
            disabled={busy || !sources.length}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
            onChange={(event) => {
              setSourceId(event.target.value);
              setConsent(false);
              setError(null);
            }}
          >
            <option value="">{t("deviceEnrollment.chooseSource")}</option>
            {sources.map((source) => (
              <option key={source.id} value={source.id}>
                {source.nickname || t("common.unnamed")} (
                {shortId(source.device_id, 8)})
              </option>
            ))}
          </select>
          {phase === "idle" && !sources.length && (
            <p className="text-xs leading-relaxed text-muted-foreground">
              {t("deviceEnrollment.noSources")}
            </p>
          )}
          {selected && (
            <p
              data-testid="enrollment-source-account"
              className="select-text break-all font-mono text-xs text-muted-foreground"
            >
              {selected.account_id}
            </p>
          )}
          <Button
            variant="secondary"
            size="sm"
            onClick={scan}
            disabled={busy}
            data-testid="enrollment-scan"
          >
            {t("deviceEnrollment.findDevices")}
          </Button>
        </div>
        <details className="text-sm">
          <summary className="cursor-pointer py-1 text-muted-foreground">
            {t("deviceEnrollment.directAddress")}
          </summary>
          <div className="mt-2 space-y-2">
            <Label htmlFor="enrollment-targets">
              {t("deviceEnrollment.addressLabel")}
            </Label>
            <Input
              id="enrollment-targets"
              placeholder="192.168.1.20:47474"
              value={targets}
              disabled={busy}
              maxLength={2048}
              onChange={(event) => setTargets(event.target.value)}
              autoComplete="off"
              spellCheck={false}
              aria-describedby="enrollment-route-hint"
            />
            <p
              id="enrollment-route-hint"
              className="text-xs leading-relaxed text-muted-foreground"
            >
              {t("deviceEnrollment.addressHint")}
            </p>
          </div>
        </details>
        <div className="space-y-2">
          <Label htmlFor="enrollment-code">{t("linkDevice.pairingCode")}</Label>
          <Input
            id="enrollment-code"
            value={code}
            onChange={(event) => setCode(event.target.value)}
            disabled={busy}
            autoComplete="off"
            spellCheck={false}
            autoCapitalize="none"
            className="font-mono tracking-wider"
            aria-describedby="enrollment-code-hint"
          />
          <p
            id="enrollment-code-hint"
            className="text-xs leading-relaxed text-muted-foreground"
          >
            {t("linkDevice.codeHint")}
          </p>
        </div>
        <label
          className="flex items-start gap-2 text-sm leading-relaxed"
          htmlFor="enrollment-history"
        >
          <input
            id="enrollment-history"
            type="checkbox"
            checked={includeHistory}
            disabled={busy}
            onChange={(event) => setIncludeHistory(event.target.checked)}
            className="mt-1 h-4 w-4 shrink-0 accent-signal"
          />
          <span>
            {t("deviceEnrollment.includeHistory")}
            <span className="mt-1 block text-xs text-muted-foreground">
              {t("deviceEnrollment.historyHint")}
            </span>
          </span>
        </label>
        <label
          className="flex items-start gap-2 text-sm leading-relaxed"
          htmlFor="enrollment-consent"
        >
          <input
            id="enrollment-consent"
            type="checkbox"
            checked={consent}
            disabled={busy || !selected}
            onChange={(event) => setConsent(event.target.checked)}
            className="mt-1 h-4 w-4 shrink-0 accent-signal"
          />
          <span>{t("deviceEnrollment.consent")}</span>
        </label>
        {error && (
          <p role="alert" className="break-words text-sm text-destructive">
            {error}
          </p>
        )}
        {busy && (
          <p
            role="status"
            className="flex items-center gap-2 text-sm text-muted-foreground"
          >
            <Loader2 className="h-4 w-4 shrink-0 animate-spin motion-reduce:animate-none" />
            {t(
              publishing
                ? "deviceEnrollment.linking"
                : "deviceEnrollment.finding",
            )}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button
            variant="ghost"
            onClick={() => changeOpen(false)}
            disabled={publishing}
            data-testid="enrollment-cancel"
          >
            {t("common.cancel")}
          </Button>
          <Button
            onClick={() => void link()}
            disabled={
              busy ||
              !selected ||
              !consent ||
              !/^[a-f0-9]{32}$/i.test(normalizedCode)
            }
            data-testid="enrollment-submit"
          >
            {t("deviceEnrollment.link")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
