import { useEffect, useLayoutEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { SettingsCategory } from "./model";
import { loadSettingsPage, type SettingsPageProps } from "./loadSettingsPage";
import type { ComponentType } from "react";
type Page = ComponentType<SettingsPageProps>;

/** Only the selected open page owns effects. Late imports cannot revive a retired view. */
export function SettingsPage({
  category,
  enabled,
  onReady,
  ...props
}: SettingsPageProps & {
  category: SettingsCategory;
  enabled: boolean;
  onReady: () => void;
}) {
  const { t } = useTranslation();
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{
    category: SettingsCategory;
    identity: string | null;
    Page: Page | null;
    failed: boolean;
  } | null>(null);
  const identity = props.controller.identity;
  useEffect(() => {
    if (!enabled) return;
    let current = true;
    setResult(null);
    void loadSettingsPage(category).then(
      (Page) => {
        if (current) setResult({ category, identity, Page, failed: false });
      },
      () => {
        if (current)
          setResult({ category, identity, Page: null, failed: true });
      },
    );
    return () => {
      current = false;
    };
  }, [category, identity, attempt, enabled]);
  const current =
    result?.category === category && result.identity === identity
      ? result
      : null;
  const Page = current?.Page;
  useLayoutEffect(() => {
    if (enabled && Page) onReady();
  }, [enabled, Page, onReady]);
  if (!enabled) return null;
  if (current?.failed)
    return (
      <div role="alert" className="text-xs text-destructive">
        <p>{t("common.loadFailed")}</p>
        <button
          type="button"
          className="underline"
          onClick={() => setAttempt((n) => n + 1)}
        >
          {t("common.retry")}
        </button>
      </div>
    );
  if (!Page)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        {t("common.loading")}
      </p>
    );
  if (category === "integrations" && !props.controller.settings) return null;
  return <Page {...props} />;
}
