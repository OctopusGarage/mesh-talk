import { FolderOpen } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { AttachmentReceiveSettings } from "../AttachmentReceiveSettings";
import { SettingsRow } from "./primitives";
import type { SettingsController } from "./useSettingsController";
export function FileSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const s = c.settings;
  return (
    <>
      <AttachmentReceiveSettings />
      <p className="pb-4 text-xs leading-relaxed text-muted-foreground">
        {t("settings.attachmentOverrides")}
      </p>
      {s && (
        <SettingsRow
          icon={<FolderOpen className="h-4 w-4" />}
          title={t("settings.downloadFolder")}
          desc={
            <>
              {t(
                s.download_dir
                  ? "settings.downloadFolderSet"
                  : "settings.downloadFolderDesc",
              )}
              {s.download_dir && (
                <p
                  className="mt-1 break-all font-mono text-[11px]"
                  title={s.download_dir}
                >
                  {s.download_dir}
                </p>
              )}
            </>
          }
          control={
            <Button
              variant="secondary"
              size="sm"
              disabled={c.saving}
              onClick={() => void c.chooseDownloadDir()}
            >
              {t(
                s.download_dir
                  ? "settings.changeFolder"
                  : "settings.chooseFolder",
              )}
            </Button>
          }
        />
      )}
    </>
  );
}
