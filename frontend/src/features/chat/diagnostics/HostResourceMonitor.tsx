import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { obs } from "@/lib/api";
import type { HostResourceSample } from "@/lib/api/diagnostics";
import {
  isNativeWindowMinimized,
  subscribeNativeWindowMinimized,
} from "@/lib/nativeWindowVisibility";
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
  const lease = useRef<Promise<number | null> | null>(null);
  const previous = useRef<{ at: number; reading: HostResourceSample } | null>(
    null,
  );
  const [sample, setSample] = useState<HostResourceSample>();
  const [cpuPercent, setCpuPercent] = useState<number | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const clear = () => {
    previous.current = null;
    setSample(undefined);
    setCpuPercent(null);
  };
  useEffect(() => {
    const hidden = () => {
      if (!document.hidden && !isNativeWindowMinimized()) return;
      release(lease);
      clear();
    };
    document.addEventListener("visibilitychange", hidden);
    const unsubscribe = subscribeNativeWindowMinimized(hidden);
    return () => {
      document.removeEventListener("visibilitychange", hidden);
      unsubscribe();
      release(lease);
    };
  }, []);
  const reset = () => {
    if (!lease.current) return;
    release(lease);
    clear();
    setUnavailable(true);
  };
  useSerialPoll(
    !unavailable,
    SAMPLE_INTERVAL_MS,
    async () => {
      const ticket: Promise<number | null> = (lease.current ??=
        Promise.resolve().then(() =>
          lease.current === ticket ? obs.startHostResourceSampling() : null,
        ));
      const id = await ticket;
      if (id === null || lease.current !== ticket) return null;
      const at = performance.now();
      const reading = await obs.hostResources(id);
      return { at, reading, ticket };
    },
    (result) => {
      if (!result || result.ticket !== lease.current) return;
      const { at, reading } = result;
      if (!reading) {
        reset();
        return;
      }
      setCpuPercent(cpuUsage(previous.current, { at, reading }));
      previous.current = { at, reading };
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
  const count = new Intl.NumberFormat(i18n.language);
  const size = (value: number | null) => {
    if (value === null) return "—";
    const [unit, scale] =
      value >= 1_000_000
        ? ["megabyte", 1_000_000]
        : value >= 1_000
          ? ["kilobyte", 1_000]
          : ["byte", 1];
    return new Intl.NumberFormat(i18n.language, {
      style: "unit",
      unit,
      maximumFractionDigits: 1,
    }).format(value / scale);
  };
  const total = (
    metric: "resident_bytes" | "disk_write_bytes" | "io_write_bytes",
  ) => {
    const values = sample.processes.map((process) => process[metric]);
    return values.some((value) => value === null)
      ? null
      : values.reduce<number>((sum, value) => sum + (value ?? 0), 0);
  };
  const rows = [
    ["cpu", cpuPercent === null ? "—" : percent.format(cpuPercent)],
    ["memory", size(total("resident_bytes"))],
    ["writes", size(total("disk_write_bytes"))],
    ["processes", count.format(sample.processes.length)],
    ["connections", count.format(sample.authenticated_channels)],
    ["sent", size(sample.sent_frame_bytes)],
    ["received", size(sample.received_frame_bytes)],
  ];
  if (sample.processes.some((process) => process.io_write_bytes !== null))
    rows.splice(3, 0, ["ioWrites", size(total("io_write_bytes"))]);
  return (
    <>
      <dl className="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1 text-xs tabular-nums">
        {rows.map(([label, value]) => (
          <div key={label} className="contents">
            <dt>{t(`diagnostics.resources.${label}`)}</dt>
            <dd>{value}</dd>
          </div>
        ))}
      </dl>
      {sample.bounded && (
        <p className="text-xs text-muted-foreground">
          {t("diagnostics.resources.bounded")}
        </p>
      )}
    </>
  );
}

function release(lease: { current: Promise<number | null> | null }) {
  const ticket = lease.current;
  lease.current = null;
  if (ticket)
    void ticket
      .then((id) =>
        id === null ? undefined : obs.stopHostResourceSampling(id),
      )
      .catch(() => {});
}

type TimedReading = { at: number; reading: HostResourceSample };

function cpuUsage(before: TimedReading | null, after: TimedReading) {
  if (!before || before.reading.bounded || after.reading.bounded) return null;
  const elapsed = after.at - before.at;
  if (elapsed <= 0 || elapsed > 2 * SAMPLE_INTERVAL_MS) return null;
  const baseline = new Map(
    before.reading.processes.map((entry) => [entry.pid, entry]),
  );
  if (baseline.size !== after.reading.processes.length) return null;
  let consumed = 0;
  for (const entry of after.reading.processes) {
    const older = baseline.get(entry.pid);
    if (!older || older.birth !== entry.birth || older.cpu_ms > entry.cpu_ms)
      return null;
    consumed += entry.cpu_ms - older.cpu_ms;
  }
  return consumed / elapsed;
}
