import { useEffect, useRef, useState } from "react";
import { Languages } from "lucide-react";
import { useTranslation } from "react-i18next";
import { dmLanguage, type DmLanguagePolicy } from "@/lib/api/language";
import { useAuth } from "@/store/auth";
import { SettingsRow, SELECT_CLASS } from "./primitives";

const LANGUAGES = ["en", "zh", "ja", "es", "fr", "de", "ko", "pt", "ru", "ar"];
function authMatches(owner: string, generation: number) {
  const auth = useAuth.getState();
  return auth.user?.id === owner && auth.generation === generation;
}

export function DmLanguageSettings() {
  const owner = useAuth((s) => s.user?.id ?? null);
  const generation = useAuth((s) => s.generation);
  return owner ? (
    <LanguageEditor
      key={`${owner}:${generation}`}
      owner={owner}
      generation={generation}
    />
  ) : null;
}

function LanguageEditor({
  owner,
  generation,
}: {
  owner: string;
  generation: number;
}) {
  const { t } = useTranslation();
  const [policy, setPolicy] = useState<DmLanguagePolicy | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<"load" | "save" | null>(null);
  const mounted = useRef(false);
  const pending = useRef(false);
  useEffect(() => {
    let active = true;
    mounted.current = true;
    setLoading(true);
    setError(null);
    void dmLanguage
      .get(owner)
      .then(
        (next) => {
          if (active && authMatches(owner, generation)) setPolicy(next);
        },
        () => {
          if (active && authMatches(owner, generation)) setError("load");
        },
      )
      .finally(() => {
        if (active && authMatches(owner, generation)) setLoading(false);
      });
    return () => {
      active = false;
      mounted.current = false;
    };
  }, [owner, generation, attempt]);

  const save = (language: string) => {
    if (pending.current || !policy || !authMatches(owner, generation)) return;
    pending.current = true;
    setSaving(true);
    setSaved(false);
    setError(null);
    const current = () => mounted.current && authMatches(owner, generation);
    void dmLanguage
      .set(owner, language || null)
      .then(
        (next) => {
          if (current()) {
            setPolicy(next);
            setSaved(true);
          }
        },
        () => {
          if (current()) setError("save");
        },
      )
      .finally(() => {
        if (current()) {
          pending.current = false;
          setSaving(false);
        }
      });
  };
  return (
    <SettingsRow
      id="setting-dm-language"
      icon={<Languages className="h-4 w-4" />}
      title={t("dmLanguage.title")}
      desc={
        <>
          <p>{t("dmLanguage.help")}</p>
          <p className="mt-1">{t("dmLanguage.localOnly")}</p>
          {error && (
            <div role="alert" className="mt-1 text-destructive">
              {t(
                error === "load"
                  ? "dmLanguage.loadFailed"
                  : "dmLanguage.saveFailed",
              )}
              {error === "load" && (
                <button
                  type="button"
                  className="ml-2 rounded-sm underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => setAttempt((value) => value + 1)}
                >
                  {t("contactVisibility.retry")}
                </button>
              )}
            </div>
          )}
          {saving || saved || loading ? (
            <p role="status" className="mt-1">
              {t(
                loading
                  ? "common.loading"
                  : saving
                    ? "settings.saving"
                    : "settings.saved",
              )}
            </p>
          ) : null}
        </>
      }
      control={
        <select
          id="setting-dm-language"
          data-testid="settings-dm-language-select"
          className={SELECT_CLASS}
          value={policy?.language ?? ""}
          disabled={loading || saving || !policy}
          onChange={(event) => save(event.target.value)}
        >
          <option value="">{t("dmLanguage.unrestricted")}</option>
          {LANGUAGES.filter(
            (language) =>
              !policy || policy.supported_languages.includes(language),
          ).map((language) => (
            <option key={language} value={language}>
              {t(`dmLanguage.languages.${language}`)}
            </option>
          ))}
        </select>
      }
    />
  );
}
