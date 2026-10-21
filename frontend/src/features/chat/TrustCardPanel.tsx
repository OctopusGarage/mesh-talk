import { useTranslation } from "react-i18next";
import { useEffect, useRef, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { trustCards, type TrustCardView } from "@/lib/api/trustCards";
import {
  checkedTrustArtifact,
  creationDateMs,
  MAX_TRUST_ARTIFACT_BYTES,
  trustDateInput,
  trustDateLabel,
} from "@/lib/trustCard";
import { errorMessage } from "@/lib/error";
import { captureRuntimeOwner } from "@/store/ownership";
import { useAuth } from "@/store/auth";

function downloadArtifact(json: string, account: string) {
  const url = URL.createObjectURL(
    new Blob([json], { type: "application/json" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = `mesh-talk-trust-${account.replace(/[^a-zA-Z0-9_-]/g, "").slice(0, 64)}.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Public signed statements and proofs. Private verification stays in the parent dialog. */
export function TrustCardPanel({
  accountId,
  own = false,
  displayName = "",
}: {
  accountId: string;
  own?: boolean;
  displayName?: string;
}) {
  const { t } = useTranslation();
  const owner = useAuth((s) => s.user?.id);
  const [card, setCard] = useState<TrustCardView | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [name, setName] = useState(displayName);
  const [bio, setBio] = useState("");
  const [date, setDate] = useState("");
  const [json, setJson] = useState("");
  const [note, setNote] = useState("");
  const [exported, setExported] = useState("");
  const [reload, setReload] = useState(0);
  const generation = useRef(0);
  useEffect(() => {
    const epoch = ++generation.current;
    const lease = captureRuntimeOwner();
    let active = true;
    setCard(null);
    setLoading(true);
    setError(null);
    setName(displayName);
    setBio("");
    setDate("");
    setJson("");
    setNote("");
    setExported("");
    setBusy(false);
    if (owner && accountId) {
      void trustCards
        .get(owner, accountId)
        .then((value) => {
          if (!active || generation.current !== epoch || !lease.current())
            return;
          setCard(value);
          if (value) {
            setName(value.display_name);
            setBio(value.bio);
            setDate(trustDateInput(value.created_at_ms));
          }
        })
        .catch((e) => {
          if (active && lease.current()) setError(errorMessage(e));
        })
        .finally(() => {
          if (active && lease.current()) setLoading(false);
        });
    } else setLoading(false);
    return () => {
      active = false;
      generation.current = epoch + 1;
    };
  }, [owner, accountId, displayName, reload]);

  const run = async (
    action: (owner: string) => Promise<string | void>,
    success: string,
    refresh = false,
  ) => {
    const lease = captureRuntimeOwner();
    const epoch = generation.current;
    if (!owner || owner !== lease.owner || busy || loading) return;
    const current = () => lease.current() && epoch === generation.current;
    setBusy(true);
    setError(null);
    setMessage("");
    try {
      const result = await action(owner);
      if (!current()) return;
      if (result) setExported(result);
      setMessage(success);
      if (refresh) setReload((value) => value + 1);
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      if (current()) setBusy(false);
    }
  };
  const disabled = busy || loading || !owner;
  return (
    <section
      className="min-w-0 space-y-3 border-t pt-4"
      aria-label={t("trustCard.signedTrustCard")}
      data-testid="trust-card-panel"
    >
      <div>
        <h3 className="font-display text-sm font-semibold">
          {t("trustCard.signedTrustCard")}
        </h3>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
          {t("trustCard.publicProfileStatementsSignedByThisAccount")}
        </p>
      </div>
      {loading ? (
        <p role="status" className="text-sm text-muted-foreground">
          {t("trustCard.loadingSignedCard")}
        </p>
      ) : (
        <>
          {!card && (
            <p className="text-sm text-muted-foreground">
              {t("trustCard.noSignedCardAvailableAccountCreationDate")}
            </p>
          )}
          {card && (
            <div
              className="space-y-2 text-sm"
              data-testid="signed-card-details"
            >
              {card.avatar &&
                /^data:image\/(png|jpeg|webp);base64,/.test(card.avatar) && (
                  <img
                    src={card.avatar}
                    alt={t("trustCard.signedProfileAvatar")}
                    className="h-12 w-12 rounded-md object-cover"
                  />
                )}
              <p className="font-medium [overflow-wrap:anywhere]">
                {card.display_name}
              </p>
              {card.bio && (
                <p className="whitespace-pre-wrap [overflow-wrap:anywhere]">
                  {card.bio}
                </p>
              )}
              <p className="text-xs text-muted-foreground">
                {card.created_at_ms === null
                  ? t("trustCard.accountCreationDateUnknown")
                  : t("trustCard.selfDeclaredDate", {
                      date: trustDateLabel(card.created_at_ms),
                    })}
              </p>
              <p className="text-xs text-muted-foreground">
                {t("trustCard.signedRevision")} {card.revision}
              </p>
            </div>
          )}
          <details className="text-sm">
            <summary className="cursor-pointer py-1 font-medium">
              {t("trustCard.accountIDAndQRCode")}
            </summary>
            <div className="mt-2 space-y-2">
              <p className="select-text break-all font-mono text-xs">
                {accountId}
              </p>
              <div className="w-fit rounded-md bg-white p-3">
                <QRCodeSVG
                  value={accountId}
                  size={132}
                  title={t("trustCard.accountIDQRCode")}
                />
              </div>
              <p className="text-xs text-muted-foreground">
                {t("trustCard.thisQRCodeContainsOnlyTheAccount")}
              </p>
              <Button
                size="sm"
                variant="outline"
                disabled={disabled}
                onClick={() =>
                  void run(async () => {
                    await navigator.clipboard.writeText(accountId);
                  }, t("trustCard.accountIDCopied"))
                }
              >
                {t("trustCard.copyAccountID")}
              </Button>
            </div>
          </details>
          {own && (
            <div className="space-y-3">
              <label className="block space-y-1 text-xs font-medium">
                {t("trustCard.signedDisplayName")}
                <Input
                  value={name}
                  maxLength={100}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
              <label className="block space-y-1 text-xs font-medium">
                {t("trustCard.bio")}
                <textarea
                  className="min-h-20 w-full rounded-md border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  value={bio}
                  maxLength={2000}
                  onChange={(e) => setBio(e.target.value)}
                />
              </label>
              <label className="block space-y-1 text-xs font-medium">
                {t("trustCard.selfdeclaredCreationDateOptional")}
                <Input
                  type="date"
                  disabled={disabled || card?.created_at_ms != null}
                  value={date}
                  onChange={(e) => setDate(e.target.value)}
                />
              </label>
              <p className="text-xs text-muted-foreground">
                {card?.created_at_ms != null
                  ? t("trustCard.thePublishedCreationDateIsFixed")
                  : t("trustCard.leaveBlankToKeepTheCreationDate")}{" "}
                {t("trustCard.publishingIncludesYourCurrentSharedAvatar")}
              </p>
              <Button
                size="sm"
                disabled={disabled || !name.trim()}
                onClick={() =>
                  void run(
                    (o) =>
                      trustCards.publish(
                        o,
                        name.trim(),
                        bio,
                        card?.created_at_ms ?? creationDateMs(date),
                      ),
                    t("trustCard.cardPublished"),
                    true,
                  )
                }
              >
                {t("trustCard.publishSignedCard")}
              </Button>
            </div>
          )}
          {card && (
            <>
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={disabled}
                  onClick={() =>
                    void run(async () => {
                      await navigator.clipboard.writeText(card.signed_json);
                    }, t("trustCard.signedCardCopied"))
                  }
                >
                  {t("trustCard.copySignedJSON")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={disabled}
                  onClick={() => downloadArtifact(card.signed_json, accountId)}
                >
                  {t("trustCard.downloadSignedCard")}
                </Button>
              </div>
              <details className="text-sm">
                <summary className="cursor-pointer py-1 font-medium">
                  {t("trustCard.introductionsAndCreationWitnesses")}
                </summary>
                <div className="mt-2 space-y-3">
                  <p className="text-xs leading-relaxed text-muted-foreground">
                    {t(
                      "trustCard.signaturesIdentifyTheIssuingAccountsEvaluateThose",
                    )}
                  </p>
                  {!card.introductions.length && !card.witnesses.length && (
                    <p className="text-xs text-muted-foreground">
                      {t("trustCard.noAttachedProofs")}
                    </p>
                  )}
                  {card.introductions.map((proof, index) => (
                    <div
                      key={`intro-${index}`}
                      className="space-y-1 border-t pt-2 text-xs"
                    >
                      <p className="break-all font-mono">
                        {t("trustCard.introductionBy")}{" "}
                        {proof.issuer_account_id}
                      </p>
                      <p className="text-muted-foreground">
                        {t(
                          proof.issuer_verified
                            ? "trustCard.issuerVerified"
                            : "trustCard.issuerUnverified",
                        )}
                      </p>
                      <p className="whitespace-pre-wrap [overflow-wrap:anywhere]">
                        {proof.note}
                      </p>
                      <p className="text-muted-foreground">
                        {new Date(proof.issued_at_ms).toLocaleString()}
                      </p>
                    </div>
                  ))}
                  {card.witnesses.map((proof, index) => (
                    <div
                      key={`witness-${index}`}
                      className="border-t pt-2 text-xs"
                    >
                      <p className="break-all font-mono">
                        {t("trustCard.creationWitnessedBy")}{" "}
                        {proof.issuer_account_id}
                      </p>
                      <p className="text-muted-foreground">
                        {t(
                          proof.issuer_verified
                            ? "trustCard.issuerVerified"
                            : "trustCard.issuerUnverified",
                        )}
                      </p>
                      <p className="text-muted-foreground">
                        {t("trustCard.observed")}{" "}
                        {new Date(proof.observed_at_ms).toLocaleString()}
                      </p>
                    </div>
                  ))}
                </div>
              </details>
              {!own && (
                <details className="text-sm">
                  <summary className="cursor-pointer py-1 font-medium">
                    {t("trustCard.createAShareableProof")}
                  </summary>
                  <div className="mt-2 space-y-2">
                    <label className="block space-y-1 text-xs font-medium">
                      {t("trustCard.introductionNote")}
                      <Input
                        value={note}
                        maxLength={1000}
                        onChange={(e) => setNote(e.target.value)}
                      />
                    </label>
                    <div className="flex flex-wrap gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={disabled || !note.trim()}
                        onClick={() =>
                          void run(
                            (o) =>
                              trustCards.introduce(o, accountId, note.trim()),
                            t("trustCard.introductionReadyToExport"),
                          )
                        }
                      >
                        {t("trustCard.createIntroduction")}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={disabled || card.created_at_ms === null}
                        onClick={() =>
                          void run(
                            (o) => trustCards.witness(o, accountId),
                            t("trustCard.creationWitnessReadyToExport"),
                          )
                        }
                      >
                        {t("trustCard.witnessCreation")}
                      </Button>
                    </div>
                    <p className="text-xs leading-relaxed text-muted-foreground">
                      {t("trustCard.thisRecordsThatYouObservedThisSigned")}
                    </p>
                  </div>
                </details>
              )}
            </>
          )}
          {exported && (
            <div className="space-y-2">
              <label className="block space-y-1 text-xs font-medium">
                {t("trustCard.signedProofJSON")}
                <textarea
                  readOnly
                  className="min-h-24 w-full rounded-md border bg-background p-2 font-mono text-xs"
                  value={exported}
                />
              </label>
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={disabled}
                  onClick={() =>
                    void run(async () => {
                      await navigator.clipboard.writeText(exported);
                    }, t("trustCard.signedProofCopied"))
                  }
                >
                  {t("trustCard.copyProofJSON")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={disabled}
                  onClick={() => downloadArtifact(exported, accountId)}
                >
                  {t("trustCard.downloadProof")}
                </Button>
              </div>
            </div>
          )}
          <details className="text-sm">
            <summary className="cursor-pointer py-1 font-medium">
              {t("trustCard.importASignedCardOrProof")}
            </summary>
            <div className="mt-2 space-y-2">
              <label className="block space-y-1 text-xs font-medium">
                {t("trustCard.signedArtifactJSON")}
                <textarea
                  value={json}
                  maxLength={MAX_TRUST_ARTIFACT_BYTES}
                  onChange={(e) => setJson(e.target.value)}
                  className="min-h-24 w-full rounded-md border bg-background p-2 font-mono text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                />
              </label>
              <p className="text-xs text-muted-foreground">
                {t("trustCard.signaturesAndAccountBindingsAreCheckedBefore")}
              </p>
              <Button
                size="sm"
                variant="outline"
                disabled={disabled || !json.trim()}
                onClick={() =>
                  void run(
                    (o) =>
                      trustCards.importArtifact(o, checkedTrustArtifact(json)),
                    t("trustCard.signedArtifactImported"),
                    true,
                  )
                }
              >
                {t("trustCard.importSignedJSON")}
              </Button>
            </div>
          </details>
        </>
      )}
      {busy && (
        <p role="status" className="text-xs text-muted-foreground">
          {t("trustCard.working")}
        </p>
      )}
      {message && (
        <p role="status" className="text-xs text-muted-foreground">
          {message}
        </p>
      )}
      {error && (
        <p
          role="alert"
          className="text-sm text-destructive [overflow-wrap:anywhere]"
        >
          {error}
        </p>
      )}
    </section>
  );
}
