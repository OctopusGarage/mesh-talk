import { useAccountProfiles } from "@/store/accountProfiles";
import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { QRCodeSVG } from "qrcode.react";
import { Button } from "@/components/ui/button";
import {
  accountProfile,
  type AccountProfileView,
} from "@/lib/api/accountProfile";
import { errorMessage } from "@/lib/error";
import { captureRuntimeOwner } from "@/store/ownership";
import { useAuth } from "@/store/auth";

/** Ordinary public profile; account verification stays in the contact dialog. */
export function AccountProfilePanel({
  accountId,
  own = false,
}: {
  accountId: string;
  own?: boolean;
}) {
  const { t } = useTranslation();
  const owner = useAuth((s) => s.user?.id);
  const sessionGeneration = useAuth((s) => s.generation);
  const cachedProfile = useAccountProfiles(
    (state) => state.profiles[accountId],
  );
  const [profile, setProfile] = useState<AccountProfileView | null>(null);
  const [bio, setBio] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [reload, setReload] = useState(0);
  const generation = useRef(0);
  const bioLimitId = useId();
  const bioBytes = new TextEncoder().encode(bio).length;
  const bioTooLong = bioBytes > 2048;
  useEffect(() => {
    const epoch = ++generation.current;
    const lease = captureRuntimeOwner();
    const current = () => generation.current === epoch && lease.current();
    setProfile(null);
    setBio("");
    setError(null);
    setMessage("");
    setBusy(false);
    setLoading(true);
    setLoadFailed(false);
    if (owner && accountId) {
      void useAccountProfiles
        .getState()
        .load(accountId)
        .then((value) => {
          if (!current()) return;
          setProfile(value);
          setBio(value?.bio ?? "");
        })
        .catch((e) => {
          if (current()) {
            setLoadFailed(true);
            setError(errorMessage(e));
          }
        })
        .finally(() => {
          if (current()) setLoading(false);
        });
    } else setLoading(false);
    return () => {
      generation.current = epoch + 1;
    };
  }, [owner, sessionGeneration, accountId, reload]);
  useEffect(() => {
    if (!own && cachedProfile !== undefined) setProfile(cachedProfile);
  }, [cachedProfile, own]);
  const run = async (
    action: () => Promise<void>,
    success: string,
    onSuccess?: () => void,
  ) => {
    const lease = captureRuntimeOwner();
    const epoch = generation.current;
    if (!owner || owner !== lease.owner || busy || loading) return;
    const current = () => generation.current === epoch && lease.current();
    setBusy(true);
    setError(null);
    setMessage("");
    try {
      await action();
      if (current()) {
        onSuccess?.();
        setMessage(success);
      }
    } catch (e) {
      if (current()) setError(errorMessage(e));
    } finally {
      if (current()) setBusy(false);
    }
  };
  const date =
    profile?.created_at_ms == null ? null : new Date(profile.created_at_ms);
  const dateLabel =
    date && Number.isFinite(date.getTime())
      ? date.toLocaleDateString()
      : t("accountProfile.unknown");
  return (
    <section
      className="min-w-0 space-y-3 border-t pt-4"
      aria-label={t("accountProfile.details")}
      data-testid="account-profile-panel"
    >
      {loading ? (
        <p role="status" className="text-sm text-muted-foreground">
          {t("accountProfile.loading")}
        </p>
      ) : (
        <>
          <div
            className="space-y-3 text-sm"
            data-testid="account-profile-details"
          >
            {own ? (
              <>
                <label className="block space-y-1 text-xs font-medium">
                  {t("accountProfile.about")}
                  <textarea
                    aria-label={t("accountProfile.about")}
                    className="min-h-20 w-full rounded-md border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    value={bio}
                    disabled={busy || loadFailed}
                    maxLength={2048}
                    aria-invalid={bioTooLong}
                    aria-describedby={bioTooLong ? bioLimitId : undefined}
                    onChange={(e) => {
                      setBio(e.target.value);
                      setMessage("");
                    }}
                  />
                </label>
                {bioTooLong && (
                  <p
                    id={bioLimitId}
                    role="alert"
                    className="text-xs text-destructive"
                  >
                    {t("accountProfile.bioTooLong", {
                      count: bioBytes,
                      max: 2048,
                    })}
                  </p>
                )}
                <Button
                  size="sm"
                  disabled={
                    busy ||
                    loadFailed ||
                    bioTooLong ||
                    bio === (profile?.bio ?? "")
                  }
                  onClick={() =>
                    void run(
                      () => accountProfile.saveBio(owner!, bio),
                      t("accountProfile.saved"),
                      () => {
                        setProfile((previous) =>
                          previous
                            ? { ...previous, bio }
                            : {
                                account_id: accountId,
                                display_name: "",
                                bio,
                                created_at_ms: null,
                              },
                        );
                      },
                    )
                  }
                >
                  {busy ? t("accountProfile.saving") : t("common.save")}
                </Button>
              </>
            ) : profile?.bio ? (
              <div className="space-y-1">
                <h3 className="text-xs font-medium text-muted-foreground">
                  {t("accountProfile.about")}
                </h3>
                <p className="whitespace-pre-wrap [overflow-wrap:anywhere]">
                  {profile.bio}
                </p>
              </div>
            ) : null}
            <dl>
              <dt className="text-xs text-muted-foreground">
                {t("accountProfile.created")}
              </dt>
              <dd className="mt-1">{dateLabel}</dd>
            </dl>
          </div>
        </>
      )}
      <details className="text-sm">
        <summary className="cursor-pointer py-1 font-medium">
          {t("accountProfile.accountId")}
        </summary>
        <div className="mt-2 space-y-2">
          <p className="select-text break-all font-mono text-xs">{accountId}</p>
          <div className="w-fit rounded-md bg-white p-3">
            <QRCodeSVG
              value={accountId}
              size={132}
              title={t("accountProfile.qrCode")}
            />
          </div>
          <Button
            size="sm"
            variant="outline"
            disabled={busy || loading || !owner}
            onClick={() =>
              void run(
                () => navigator.clipboard.writeText(accountId),
                t("accountProfile.copied"),
              )
            }
          >
            {t("accountProfile.copyId")}
          </Button>
        </div>
      </details>
      {message && (
        <p role="status" className="text-xs text-muted-foreground">
          {message}
        </p>
      )}
      {error && (
        <div className="space-y-2">
          <p
            role="alert"
            className="text-sm text-destructive [overflow-wrap:anywhere]"
          >
            {error}
          </p>
          {loadFailed && (
            <Button
              size="sm"
              variant="outline"
              onClick={() => setReload((value) => value + 1)}
            >
              {t("accountProfile.retry")}
            </Button>
          )}
        </div>
      )}
    </section>
  );
}
