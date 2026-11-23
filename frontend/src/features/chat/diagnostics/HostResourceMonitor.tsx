import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { obs } from "@/lib/api";
import type {
  HostResourceSample,
  ProcessResourceReading,
} from "@/lib/api/diagnostics";
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
  const [interval, setIntervalReading] = useState<IntervalReading>();
  const [memoryPeak, setMemoryPeak] = useState<number | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const clear = () => {
    previous.current = null;
    setSample(undefined);
    setIntervalReading(undefined);
    setMemoryPeak(null);
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
      setIntervalReading(compareReadings(previous.current, { at, reading }));
      if (!reading.bounded) {
        const resident = total(reading.processes, "resident_bytes");
        if (resident !== null)
          setMemoryPeak((peak) => Math.max(peak ?? 0, resident));
      }
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
  const rate = (bytes: number | null | undefined) =>
    bytes == null
      ? "—"
      : t("diagnostics.resources.perSecond", { size: size(bytes) });
  const percentValue = (value: number | null | undefined) =>
    value == null ? "—" : percent.format(value);
  const rows = [
    ["cpu", percentValue(interval?.cpu)],
    ["memory", size(total(sample.processes, "resident_bytes"))],
    ["memoryPeak", size(memoryPeak)],
    ["writes", size(total(sample.processes, "disk_write_bytes"))],
    ["diskRate", rate(interval?.disk)],
    ["connections", count.format(sample.authenticated_channels)],
    ["sent", size(sample.sent_frame_bytes)],
    ["received", size(sample.received_frame_bytes)],
    ["sendRate", rate(interval?.sent)],
    ["receiveRate", rate(interval?.received)],
  ];
  if (sample.processes.some((process) => process.io_write_bytes !== null))
    rows.push(
      ["ioWrites", size(total(sample.processes, "io_write_bytes"))],
      ["ioRate", rate(interval?.io)],
    );
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
      <details className="text-xs">
        <summary className="cursor-pointer rounded-sm focus-visible:outline-2">
          {t("diagnostics.resources.processDetails", {
            count: sample.processes.length,
          })}
        </summary>
        <ul className="mt-2 grid gap-1 tabular-nums">
          {sample.processes.map((process, index) => (
            <li
              key={process.birth + ":" + process.pid}
              className="flex flex-wrap justify-between gap-x-2"
            >
              <span>
                {t(
                  index === 0
                    ? "diagnostics.resources.hostPid"
                    : "diagnostics.resources.childPid",
                  { pid: process.pid },
                )}
              </span>
              <span>
                {percentValue(interval?.processCpu.get(process.pid))} ·{" "}
                {size(process.resident_bytes)}
              </span>
            </li>
          ))}
        </ul>
      </details>
      <p className="text-xs text-muted-foreground">
        {t("diagnostics.resources.scope")}
      </p>
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

type ProcessMetric = "resident_bytes" | "disk_write_bytes" | "io_write_bytes";

function total(processes: ProcessResourceReading[], metric: ProcessMetric) {
  let sum = 0;
  for (const process of processes) {
    const value = process[metric];
    if (value === null) return null;
    sum += value;
  }
  return sum;
}

interface IntervalReading {
  cpu: number | null;
  disk: number | null;
  io: number | null;
  sent: number | null;
  received: number | null;
  processCpu: Map<number, number>;
}

function compareReadings(
  before: TimedReading | null,
  after: TimedReading,
): IntervalReading | undefined {
  if (!before) return;
  const elapsed = after.at - before.at;
  if (elapsed <= 0 || elapsed > 2 * SAMPLE_INTERVAL_MS) return;
  const older = before.reading;
  const latest = after.reading;
  const delta = (first: number | null, next: number | null) =>
    first === null || next === null || next < first ? null : next - first;
  const perSecond = (amount: number | null) =>
    amount === null ? null : (amount * 1000) / elapsed;
  const baseline = new Map(older.processes.map((entry) => [entry.pid, entry]));
  const processCpu = new Map<number, number>();
  let complete =
    !older.bounded &&
    !latest.bounded &&
    baseline.size === latest.processes.length;
  let cpu = 0;
  let disk: number | null = 0;
  let io: number | null = 0;
  const add = (sum: number | null, amount: number | null) =>
    sum === null || amount === null ? null : sum + amount;
  for (const entry of latest.processes) {
    const prior = baseline.get(entry.pid);
    if (!prior || prior.birth !== entry.birth) {
      complete = false;
      continue;
    }
    const consumed = delta(prior.cpu_ms, entry.cpu_ms);
    if (consumed === null) complete = false;
    else {
      processCpu.set(entry.pid, consumed / elapsed);
      cpu += consumed;
    }
    disk = add(disk, delta(prior.disk_write_bytes, entry.disk_write_bytes));
    io = add(io, delta(prior.io_write_bytes, entry.io_write_bytes));
  }
  return {
    cpu: complete ? cpu / elapsed : null,
    disk: complete ? perSecond(disk) : null,
    io: complete ? perSecond(io) : null,
    sent: perSecond(delta(older.sent_frame_bytes, latest.sent_frame_bytes)),
    received: perSecond(
      delta(older.received_frame_bytes, latest.received_frame_bytes),
    ),
    processCpu,
  };
}
