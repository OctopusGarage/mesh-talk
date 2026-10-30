import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useChat, displayName } from "@/store/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import { errorMessage } from "@/lib/error";
import { readWithDeadline } from "@/lib/readDeadline";
import {
  messageForwarding,
  type ForwardRequest,
  type ForwardResult,
  type ForwardBatch,
  type ForwardTarget,
  type ForwardSource,
} from "@/lib/api/messageForwarding";
import type { FavoriteMessage } from "@/lib/api/messageFavorites";
import { canParticipateChannel } from "../channelPermissions";
import { useSerialPoll } from "../useSerialPoll";

const targetKey = (target: ForwardTarget) => `${target.kind}:${target.target}`;
export function ForwardDialog({
  source,
  onClose,
  onAccepted,
}: {
  source: ForwardSource;
  onClose: () => void;
  onAccepted: (result: ForwardResult) => void;
}) {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts),
    channels = useChat((s) => s.channels),
    favorites = useChat((s) => s.favorites);
  const [query, setQuery] = useState("");
  const [targets, setTargets] = useState<ForwardTarget[]>([]);
  const [mode, setMode] = useState<ForwardRequest["mode"]>("individual");
  const [preview, setPreview] = useState<FavoriteMessage[] | null>(null);
  const [inspecting, setInspecting] = useState(true);
  const [error, setError] = useState<string | null>(null),
    [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ForwardResult | null>(null),
    [resumable, setResumable] = useState<ForwardBatch[]>([]);
  const [request, setRequest] = useState<ForwardRequest | null>(null);
  const alive = useRef(true);
  const inspection = useRef(0);
  const observation = useRef(0);
  const [runtime] = useState(captureRuntimeOwner);
  const [listed, setListed] = useState(false);
  useEffect(() => {
    alive.current = true;
    const lease = captureRuntimeOwner();
    const inspectionId = ++inspection.current;
    if (!runtime.current() || !lease.owner || !lease.current()) return;
    setPreview(null);
    setInspecting(true);
    void readWithDeadline(() => messageForwarding.inspect(lease.owner!, source))
      .then((value) => {
        if (
          alive.current &&
          lease.current() &&
          inspection.current === inspectionId
        )
          setPreview(value.messages);
      })
      .catch((e) => {
        if (
          alive.current &&
          lease.current() &&
          inspection.current === inspectionId
        )
          setError(errorMessage(e));
      })
      .finally(() => {
        if (
          alive.current &&
          lease.current() &&
          inspection.current === inspectionId
        )
          setInspecting(false);
      });
    return () => {
      alive.current = false;
    };
  }, [source, runtime]);
  const pending =
    result?.items.filter((item) => item.status === "pending").length ?? 0;
  useSerialPoll(
    !listed ||
      (request !== null && result === null) ||
      pending > 0 ||
      resumable.some((batch) =>
        batch.result.items.some((item) => item.status === "pending"),
      ),
    2000,
    async () => {
      const revision = observation.current;
      const batches =
        runtime.owner && runtime.current()
          ? await messageForwarding.list(runtime.owner)
          : [];
      return { batches, revision };
    },
    ({ batches, revision }) => {
      if (
        !alive.current ||
        !runtime.current() ||
        revision !== observation.current
      )
        return;
      setListed(true);
      setResumable(
        batches.filter(
          (batch) =>
            batch.request.source.kind === source.kind &&
            batch.request.source.target === source.target &&
            batch.result.items.some((item) => item.status !== "accepted"),
        ),
      );
      // Observing admitted work never reads the source or submits another request.
      const observed =
        request &&
        batches.find(
          (batch) =>
            batch.request.operation_id === request.operation_id &&
            batch.result.operation_id === request.operation_id,
        );
      if (
        observed &&
        JSON.stringify(observed.result) !== JSON.stringify(result)
      ) {
        setResult(observed.result);
        onAccepted(observed.result);
      }
    },
    (error) => {
      if (!alive.current || !runtime.current()) return;
      setListed(true);
      setError(errorMessage(error));
    },
  );
  const choices: Array<ForwardTarget & { name: string }> = [
    ...accounts.map((account) => ({
      kind: "account" as const,
      target: account.account_id,
      name: displayName(
        favorites,
        account.account_id,
        account.names[0] ?? account.account_id,
      ),
    })),
    ...channels.filter(canParticipateChannel).map((channel) => ({
      kind: "channel" as const,
      target: channel.channel_id,
      name: channel.name,
    })),
  ];
  const nameOf = (target: ForwardTarget) =>
    choices.find((choice) => targetKey(choice) === targetKey(target))?.name ??
    target.target;
  const send = async (resume?: ForwardRequest) => {
    const lease = captureRuntimeOwner();
    if (!runtime.current() || !lease.owner || !lease.current() || busy) return;
    const next = resume ??
      request ?? {
        operation_id: `${Date.now()}-${crypto.randomUUID()}`,
        source,
        destinations: targets,
        mode,
      };
    setRequest(next);
    // Ignore observations started before or during this mutation's response.
    observation.current++;
    setBusy(true);
    setError(null);
    try {
      const value = await messageForwarding.send(lease.owner, next);
      if (!alive.current || !lease.current()) return;
      observation.current++;
      setResult(value);
      onAccepted(value);
    } catch (e) {
      if (alive.current && lease.current()) setError(errorMessage(e));
    } finally {
      if (alive.current && lease.current()) setBusy(false);
    }
  };
  const failed =
    result?.items.filter((item) => item.status === "failed").length ?? 0;
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] w-[calc(100vw-2rem)] min-w-0 max-w-lg overflow-x-hidden overflow-y-auto"
        data-testid="forward-dialog"
      >
        <DialogHeader>
          <DialogTitle>{t("messageForwarding.title")}</DialogTitle>
          <DialogDescription>
            {t("messageForwarding.description", {
              count: (request?.source ?? source).message_ids.length,
            })}
          </DialogDescription>
        </DialogHeader>
        {error && (
          <p role="alert" className="break-words text-sm text-destructive">
            {error}
          </p>
        )}
        <div
          className="max-h-32 space-y-1 overflow-y-auto rounded-md border bg-muted/30 p-3 text-sm"
          aria-label={t("messageForwarding.preview")}
        >
          {preview ? (
            preview.map((message) => (
              <p key={message.id} className="truncate">
                <span className="text-muted-foreground">
                  {message.sender_name || message.who}:{" "}
                </span>
                {message.file?.name || message.text || message.sticker}
              </p>
            ))
          ) : inspecting ? (
            <p role="status">{t("common.loading")}</p>
          ) : null}
        </div>
        {!request ? (
          <>
            {source.message_ids.length > 1 && (
              <fieldset className="flex flex-wrap gap-4 text-sm">
                <legend className="sr-only">
                  {t("messageForwarding.mode")}
                </legend>
                {(["individual", "merged"] as const).map((value) => (
                  <label
                    key={value}
                    className="flex min-h-11 items-center gap-2"
                  >
                    <input
                      type="radio"
                      name="forward-mode"
                      checked={mode === value}
                      onChange={() => setMode(value)}
                    />
                    {t(`messageForwarding.${value}`)}
                  </label>
                ))}
              </fieldset>
            )}
            <label className="grid gap-2 text-sm">
              {t("messageForwarding.recipients")}
              <input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("messageForwarding.search")}
                className="h-11 w-full rounded-md border bg-background px-3"
              />
            </label>
            <div className="max-h-52 overflow-y-auto rounded-md border">
              {choices
                .filter((choice) =>
                  choice.name
                    .toLocaleLowerCase()
                    .includes(query.toLocaleLowerCase()),
                )
                .map((choice) => {
                  const checked = targets.some(
                    (target) => targetKey(target) === targetKey(choice),
                  );
                  return (
                    <label
                      key={targetKey(choice)}
                      className="flex min-h-11 cursor-pointer items-center gap-3 border-b px-3 py-2 last:border-0 hover:bg-accent"
                    >
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={!checked && targets.length >= 5}
                        onChange={() =>
                          setTargets((previous) =>
                            checked
                              ? previous.filter(
                                  (target) =>
                                    targetKey(target) !== targetKey(choice),
                                )
                              : [
                                  ...previous,
                                  { kind: choice.kind, target: choice.target },
                                ],
                          )
                        }
                      />
                      <span className="min-w-0 flex-1 truncate text-sm">
                        {choice.name}
                      </span>
                      <span className="text-xs text-muted-foreground">
                        {t(`messageForwarding.${choice.kind}`)}
                      </span>
                    </label>
                  );
                })}
              {!choices.some((choice) =>
                choice.name
                  .toLocaleLowerCase()
                  .includes(query.toLocaleLowerCase()),
              ) && (
                <p className="p-3 text-sm text-muted-foreground">
                  {t("messageForwarding.noRecipients")}
                </p>
              )}
            </div>
            <p
              className="min-w-0 break-words text-sm text-muted-foreground"
              role="status"
            >
              {t("messageForwarding.confirm", {
                count: source.message_ids.length,
                recipients: targets.length,
              })}{" "}
              {targets.map(nameOf).join(", ")}
            </p>
            {preview?.some(
              (message) =>
                message.expires_at_ms != null &&
                (mode === "merged" || !!message.file),
            ) && (
              <p role="alert" className="text-sm text-destructive">
                {t("messageForwarding.expiryUnsupported")}
              </p>
            )}
            <Button
              onClick={() => void send()}
              disabled={
                !preview ||
                !targets.length ||
                busy ||
                preview.some(
                  (message) =>
                    message.expires_at_ms != null &&
                    (mode === "merged" || !!message.file),
                )
              }
            >
              {t("messageForwarding.send")}
            </Button>
            {resumable.length > 0 && (
              <div className="grid gap-2 border-t pt-3">
                <p className="text-sm font-medium">
                  {t("messageForwarding.unfinished")}
                </p>
                {resumable.map((batch) => (
                  <Button
                    variant="outline"
                    key={batch.request.operation_id}
                    onClick={() => {
                      setTargets(batch.request.destinations);
                      setMode(batch.request.mode);
                      setRequest(batch.request);
                      setResult(batch.result);
                      const inspectionId = ++inspection.current;
                      setPreview(null);
                      setInspecting(true);
                      setError(null);
                      const lease = captureRuntimeOwner();
                      if (lease.owner && lease.current())
                        void readWithDeadline(() =>
                          messageForwarding.inspect(
                            lease.owner!,
                            batch.request.source,
                          ),
                        )
                          .then((value) => {
                            if (
                              alive.current &&
                              lease.current() &&
                              inspection.current === inspectionId
                            )
                              setPreview(value.messages);
                          })
                          .catch((e) => {
                            if (
                              alive.current &&
                              lease.current() &&
                              inspection.current === inspectionId
                            )
                              setError(errorMessage(e));
                          })
                          .finally(() => {
                            if (
                              alive.current &&
                              lease.current() &&
                              inspection.current === inspectionId
                            )
                              setInspecting(false);
                          });
                    }}
                  >
                    {t("messageForwarding.resume", {
                      count: batch.result.items.filter(
                        (item) => item.status !== "accepted",
                      ).length,
                    })}
                  </Button>
                ))}
              </div>
            )}
          </>
        ) : (
          <>
            <div role="status" className="text-sm">
              {busy && !result
                ? t("messageForwarding.sending")
                : t("messageForwarding.summary", {
                    accepted:
                      result?.items.filter((item) => item.status === "accepted")
                        .length ?? 0,
                    failed,
                  })}
              {pending > 0 && (
                <>
                  {" "}
                  · {t("messageForwarding.pendingCount", { count: pending })}
                </>
              )}
            </div>
            <p className="break-words text-sm text-muted-foreground">
              {t(`messageForwarding.${request.mode}`)} ·{" "}
              {request.destinations.map(nameOf).join(", ")}
            </p>
            {result && (
              <ul className="max-h-64 space-y-2 overflow-y-auto">
                {result.items.map((item, index) => (
                  <li
                    key={`${targetKey(item.target)}:${item.source_id}:${index}`}
                    className="rounded-md border p-3 text-sm"
                  >
                    <span className="font-medium">{nameOf(item.target)}</span>
                    <span className="ml-2 text-muted-foreground">
                      {t(`messageForwarding.${item.status}`)}
                    </span>
                    {item.source_id && (
                      <p className="truncate text-xs text-muted-foreground">
                        {preview?.find(
                          (message) => message.id === item.source_id,
                        )?.file?.name ||
                          preview?.find(
                            (message) => message.id === item.source_id,
                          )?.text ||
                          item.source_id}
                      </p>
                    )}
                    {item.error && (
                      <p className="mt-1 break-words text-destructive">
                        {item.error}
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            )}
            {!busy && (!result || failed > 0) && (
              <Button disabled={!preview} onClick={() => void send()}>
                {t("messageForwarding.retry")}
              </Button>
            )}
            <Button variant="outline" onClick={onClose}>
              {t("common.close")}
            </Button>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
