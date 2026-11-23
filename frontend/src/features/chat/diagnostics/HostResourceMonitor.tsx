import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { obs } from "@/lib/api";
import type { HostResourceSample } from "@/lib/api/diagnostics";
import { useSerialPoll } from "../useSerialPoll";

const SAMPLE_INTERVAL_MS = 3000;

export function HostResourceMonitor() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  return (
    <>
      <p className="text-xs text-muted-foreground">
        {t("diagnostics.resources.description")}
      </p>
      <Button
        size="sm"
        variant="outline"
        aria-pressed={enabled}
        onClick={() => setEnabled((active) => !active)}
      >
        {t(
          enabled
            ? "diagnostics.resources.stop"
            : "diagnostics.resources.start",
        )}
      </Button>
      {enabled && <HostResourceReadings />}
    </>
  );
}

function HostResourceReadings() {
  const { t, i18n } = useTranslation();
  const previous = useRef<{ at: number; cpuMs: number } | null>(null);
  const [sample, setSample] = useState<HostResourceSample>();
  const [cpuPercent, setCpuPercent] = useState<number | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const reset = () => {
    previous.current = null;
    setSample(undefined);
    setCpuPercent(null);
    setUnavailable(true);
  };
  useSerialPoll(
    !unavailable,
    SAMPLE_INTERVAL_MS,
    () => {
      const at = performance.now();
      return obs.hostResources().then((reading) => ({ at, reading }));
    },
    ({ at, reading }) => {
      if (!reading) {
        reset();
        return;
      }
      const before = previous.current;
      const elapsed = before ? at - before.at : 0;
      const consumed = before ? reading.cpu_ms - before.cpuMs : -1;
      setCpuPercent(
        elapsed > 0 && elapsed <= 2 * SAMPLE_INTERVAL_MS && consumed >= 0
          ? consumed / elapsed
          : null,
      );
      previous.current = { at, cpuMs: reading.cpu_ms };
      setSample(reading);
    },
    reset,
  );
  if (unavailable)
    return <p role="status">{t("diagnostics.resources.unavailable")}</p>;
  if (!sample) return <p role="status">{t("common.loading")}</p>;
  const percent = new Intl.NumberFormat(i18n.language, {
    style: "percent",
    maximumFractionDigits: 2,
  });
  const megabytes = new Intl.NumberFormat(i18n.language, {
    style: "unit",
    unit: "megabyte",
    maximumFractionDigits: 1,
  });
  const missing = "—";
  const bytes = (value: number | null) =>
    value == null ? missing : megabytes.format(value / 1_000_000);
  return (
    <dl className="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1 text-xs tabular-nums">
      <dt>{t("diagnostics.resources.cpu")}</dt>
      <dd>{cpuPercent === null ? missing : percent.format(cpuPercent)}</dd>
      <dt>{t("diagnostics.resources.memory")}</dt>
      <dd>{bytes(sample.resident_bytes)}</dd>
      <dt>{t("diagnostics.resources.writes")}</dt>
      <dd>{bytes(sample.disk_write_bytes)}</dd>
    </dl>
  );
}
