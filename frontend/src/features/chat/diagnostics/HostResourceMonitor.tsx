import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { obs } from "@/lib/api";
import type {
  HostResourceSample,
  MessageResourceCounters,
  ProcessResourceReading,
} from "@/lib/api/diagnostics";
import {
  isNativeWindowMinimized,
  subscribeNativeWindowMinimized,
} from "@/lib/nativeWindowVisibility";
import { useSerialPoll } from "../useSerialPoll";

const SAMPLE_INTERVAL_MS = 3000;
const DISCLOSURE_CLASS =
  "cursor-pointer rounded-lg px-3 py-2 font-medium marker:text-muted-foreground hover:bg-muted/40 focus-visible:outline-2 focus-visible:outline-ring motion-safe:transition-colors";

export function HostResourceMonitor() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  return (
    <>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="min-w-0 flex-1 basis-52 text-xs leading-relaxed text-muted-foreground">
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
      </div>
      {enabled && <HostResourceReadings />}
    </>
  );
}

function HostResourceReadings() {
  const { t, i18n } = useTranslation();
  const [processDetailsOpen, setProcessDetailsOpen] = useState(false);
  const [messageDetailsOpen, setMessageDetailsOpen] = useState(false);
  const { percent, count, byte, kilobyte, megabyte } = useMemo(() => {
    const formatBytes = (unit: string) =>
      new Intl.NumberFormat(i18n.language, {
        style: "unit",
        unit,
        maximumFractionDigits: 1,
      });
    return {
      percent: new Intl.NumberFormat(i18n.language, {
        style: "percent",
        maximumFractionDigits: 2,
      }),
      count: new Intl.NumberFormat(i18n.language),
      byte: formatBytes("byte"),
      kilobyte: formatBytes("kilobyte"),
      megabyte: formatBytes("megabyte"),
    };
  }, [i18n.language]);
  const lease = useRef<Promise<number | null> | null>(null);
  const previous = useRef<HostResourceSample | null>(null);
  const [sample, setSample] = useState<HostResourceSample>();
  const [interval, setIntervalReading] = useState<IntervalReading>();
  const [memoryPeak, setMemoryPeak] = useState<number | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const clear = () => {
    previous.current = null;
    setSample(undefined);
    setIntervalReading(undefined);
    setMemoryPeak(null);
    setProcessDetailsOpen(false);
    setMessageDetailsOpen(false);
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
      const reading = await obs.hostResources(id, messageDetailsOpen);
      return { reading, ticket };
    },
    (result) => {
      if (!result || result.ticket !== lease.current) return;
      const { reading } = result;
      if (!reading) {
        reset();
        return;
      }
      setIntervalReading(compareReadings(previous.current, reading));
      if (!reading.bounded) {
        const resident = total(reading.processes, "resident_bytes");
        if (resident !== null)
          setMemoryPeak((peak) => Math.max(peak ?? 0, resident));
      }
      previous.current = reading;
      setSample(reading);
    },
    reset,
  );
  if (unavailable)
    return <p role="status">{t("diagnostics.resources.unavailable")}</p>;
  if (!sample) return <p role="status">{t("common.loading")}</p>;
  const size = (value: number | null) => {
    if (value === null) return "—";
    if (value >= 1_000_000) return megabyte.format(value / 1_000_000);
    if (value >= 1_000) return kilobyte.format(value / 1_000);
    return byte.format(value);
  };
  const rate = (bytes: number | null | undefined) =>
    bytes == null
      ? "—"
      : t("diagnostics.resources.perSecond", { size: size(bytes) });
  const percentValue = (value: number | null | undefined) =>
    value == null ? "—" : percent.format(value);
  const system = [
    ["cpu", percentValue(interval?.cpu)],
    ["memory", size(total(sample.processes, "resident_bytes"))],
    ["memoryPeak", size(memoryPeak)],
  ];
  const ioAvailable = sample.processes.some(
    (process) => process.io_write_bytes !== null,
  );
  const disk = ioAvailable
    ? [
        ["ioWrites", size(total(sample.processes, "io_write_bytes"))],
        ["ioRate", rate(interval?.io)],
      ]
    : [
        ["writes", size(total(sample.processes, "disk_write_bytes"))],
        ["diskRate", rate(interval?.disk)],
      ];
  const network = [
    ["connections", count.format(sample.authenticated_channels)],
    ["sendRate", rate(interval?.sent)],
    ["receiveRate", rate(interval?.received)],
    ["sent", size(sample.sent_frame_bytes)],
    ["received", size(sample.received_frame_bytes)],
  ];
  const labels = (rows: string[][]) =>
    rows.map(([label, value]) => ({
      label: t(`diagnostics.resources.${label}`),
      value,
    }));
  return (
    <div className="grid gap-2.5">
      <div className="grid gap-2.5 sm:grid-cols-2">
        <MetricGroup
          title={t("diagnostics.resources.sections.compute")}
          metrics={labels(system)}
        />
        <MetricGroup
          title={t("diagnostics.resources.sections.storage")}
          metrics={labels(disk)}
        />
      </div>
      <MetricGroup
        title={t("diagnostics.resources.sections.traffic")}
        metrics={labels(network)}
      />
      <details
        className="rounded-lg border border-border/65 text-xs"
        onToggle={(event) => setProcessDetailsOpen(event.currentTarget.open)}
      >
        <summary className={DISCLOSURE_CLASS}>
          {t("diagnostics.resources.processDetails", {
            count: sample.processes.length,
          })}
        </summary>
        {processDetailsOpen && (
          <ul className="grid gap-1.5 border-t border-border/65 px-3 py-2 tabular-nums">
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
        )}
      </details>
      {sample.messages && (
        <details
          className="rounded-lg border border-border/65 text-xs"
          onToggle={(event) => setMessageDetailsOpen(event.currentTarget.open)}
        >
          <summary className={DISCLOSURE_CLASS}>
            {t("diagnostics.resources.messages.title")}
          </summary>
          {messageDetailsOpen && (
            <MessageReadings
              metrics={sample.messages}
              backlog={sample.direct_backlog}
              formatSize={size}
              formatNumber={count.format}
              verifiedRate={rate(interval?.verifiedBytes)}
            />
          )}
        </details>
      )}
      <p className="text-xs leading-relaxed text-muted-foreground">
        {sample.bounded && (
          <strong className="mr-1 font-medium text-foreground">
            {t("diagnostics.resources.bounded")}
          </strong>
        )}
        {t("diagnostics.resources.scope")}
      </p>
    </div>
  );
}

const RECEIVED_TYPES = ["text", "sticker", "media", "attachment"] as const;
const LATENCY_BUCKET_MS = [50, 100, 250, 500, 1000, 3000, 10000];

function MessageReadings({
  metrics,
  backlog,
  formatSize,
  verifiedRate,
  formatNumber,
}: {
  metrics: MessageResourceCounters;
  backlog: HostResourceSample["direct_backlog"];
  formatSize: (bytes: number | null) => string;
  verifiedRate: string;
  formatNumber: (value: number) => string;
}) {
  const { t } = useTranslation();
  const formatCount = (value?: number) =>
    value === undefined ? "—" : formatNumber(value);
  const milliseconds = (value: number) =>
    t("diagnostics.resources.messages.milliseconds", {
      value: formatNumber(value),
    });
  const samples = metrics.delivery_latency.reduce(
    (sum, value) => sum + value,
    0,
  );
  const percentile = (fraction: number) => {
    if (samples === 0) return "—";
    let remaining = Math.ceil(samples * fraction);
    for (const [index, bucket] of metrics.delivery_latency.entries()) {
      remaining -= bucket;
      if (remaining <= 0) {
        const ceiling = LATENCY_BUCKET_MS[index];
        return ceiling === undefined
          ? "> " + milliseconds(10_000)
          : "≤ " + milliseconds(ceiling);
      }
    }
    return "—";
  };
  const groups = {
    delivery: [
      ["accepted", formatCount(metrics.accepted)],
      ["delivered", formatCount(metrics.delivered)],
      ["awaiting", formatCount(backlog?.[0])],
      ["oldest", backlog?.[1] == null ? "—" : milliseconds(backlog[1])],
      ["p50", percentile(0.5)],
      ["p95", percentile(0.95)],
      ["latencySamples", formatCount(samples)],
    ],
    recovery: [
      ["requestErrors", formatCount(metrics.send_request_errors)],
      ["rejected", formatCount(metrics.rejected)],
      ["timeouts", formatCount(metrics.attempt_timeouts)],
      ["retries", formatCount(metrics.retries_scheduled)],
      ["routeRestarts", formatCount(metrics.route_restarts)],
      ["directStorage", formatCount(metrics.direct_storage_holds)],
      ["relayStorage", formatCount(metrics.relay_storage_holds)],
    ],
    sync: [
      ["catchUp", formatCount(metrics.catch_up)],
      ["historySync", formatCount(metrics.history_sync)],
      ["ownDevice", formatCount(metrics.own_device_sync)],
      ["duplicateDirect", formatCount(metrics.duplicate_direct_events)],
    ],
    attachments: [
      ["attachmentAccepted", formatCount(metrics.attachments_accepted)],
      ["attachmentDeclined", formatCount(metrics.attachments_declined)],
      ["verified", formatCount(metrics.attachments_verified)],
      ["verifiedBytes", formatSize(metrics.verified_attachment_bytes)],
      ["verifiedRate", verifiedRate],
    ],
  };
  const group = (key: keyof typeof groups) => (
    <MetricGroup
      title={t(`diagnostics.resources.sections.${key}`)}
      metrics={groups[key].map(([label, value]) => ({
        label: t(`diagnostics.resources.messages.${label}`),
        value,
      }))}
    />
  );
  return (
    <div className="grid gap-2.5 border-t border-border/65 bg-muted/20 p-2.5">
      {group("delivery")}
      <div className="overflow-x-auto rounded-lg border border-border/65 bg-background/50 px-3 py-2">
        <table className="w-full text-left tabular-nums [&_td]:py-1 [&_th]:pr-2 [&_tbody_tr]:border-t [&_tbody_tr]:border-border/55">
          <caption className="mb-1.5 text-left font-medium">
            {t("diagnostics.resources.messages.received")}
          </caption>
          <thead>
            <tr>
              <th scope="col">{t("diagnostics.resources.messages.type")}</th>
              <th scope="col">{t("diagnostics.resources.messages.direct")}</th>
              <th scope="col">{t("diagnostics.resources.messages.group")}</th>
            </tr>
          </thead>
          <tbody>
            {RECEIVED_TYPES.map((kind, index) => (
              <tr key={kind}>
                <th scope="row" className="font-normal">
                  {t(`diagnostics.resources.messages.${kind}`)}
                </th>
                {[
                  metrics.received_direct[index],
                  metrics.received_group[index],
                ].map((bucket, scope) => (
                  <td key={scope}>
                    {bucket
                      ? `${formatCount(bucket.count)} · ${formatSize(bucket.bytes)}`
                      : "—"}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {group("recovery")}
      {group("sync")}
      {group("attachments")}
      <p className="leading-relaxed text-muted-foreground">
        {t("diagnostics.resources.messages.scope")}
      </p>
    </div>
  );
}

function MetricGroup({
  title,
  metrics,
}: {
  title: string;
  metrics: Array<{ label: string; value: string }>;
}) {
  return (
    <section className="min-w-0 rounded-lg border border-border/65 bg-background/50 px-3 py-2 text-xs">
      <h4 className="mb-1.5 font-medium">{title}</h4>
      <dl className="grid grid-cols-[minmax(0,1fr)_auto] items-baseline gap-x-3 gap-y-1 tabular-nums">
        {metrics.map(({ label, value }) => (
          <div key={label} className="contents">
            <dt className="break-words text-muted-foreground">{label}</dt>
            <dd className="text-right font-medium">{value}</dd>
          </div>
        ))}
      </dl>
    </section>
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
  verifiedBytes: number | null;
}

function compareReadings(
  older: HostResourceSample | null,
  latest: HostResourceSample,
): IntervalReading | undefined {
  if (!older) return;
  const elapsed = latest.sampled_at_ms - older.sampled_at_ms;
  if (!(elapsed > 0 && elapsed <= 2 * SAMPLE_INTERVAL_MS)) return;
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
    verifiedBytes: perSecond(
      delta(
        older.messages?.verified_attachment_bytes ?? null,
        latest.messages?.verified_attachment_bytes ?? null,
      ),
    ),
    processCpu,
  };
}
