import { useEffect, useRef, useState, type ReactNode } from "react";
import { BookOpen } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { HELP_VERSION } from "./metadata";
import { loadHelpReader } from "./helpReaderLoader";
import type { HelpReader } from "./HelpReader";
import "./help.css";

export function HelpDialog({
  open,
  onOpenChange,
  onNavigate,
  trigger,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onNavigate?: (topicId: string) => void;
  trigger?: ReactNode;
}) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("first-run");
  const [Reader, setReader] = useState<typeof HelpReader | null>(null);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const navigating = useRef(false);
  useEffect(() => {
    if (!open) return;
    navigating.current = false;
    if (Reader) return;
    let current = true;
    setFailed(false);
    void loadHelpReader().then(
      (module) => {
        if (current) setReader(() => module.HelpReader);
      },
      () => {
        if (current) setFailed(true);
      },
    );
    return () => {
      current = false;
    };
  }, [open, Reader, attempt]);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {trigger && <DialogTrigger asChild>{trigger}</DialogTrigger>}
      <DialogContent
        data-testid="help-dialog"
        className="help-dialog"
        onOpenAutoFocus={(event) => {
          if (Reader) event.preventDefault();
        }}
        onCloseAutoFocus={(event) => {
          if (navigating.current) event.preventDefault();
        }}
      >
        <DialogHeader className="pr-8">
          <DialogTitle className="flex items-center gap-2">
            <BookOpen className="h-4 w-4" />
            {t("help.title")}
          </DialogTitle>
          <DialogDescription>
            {t("help.description", { version: HELP_VERSION })}
          </DialogDescription>
        </DialogHeader>
        {open &&
          (Reader ? (
            <Reader
              query={query}
              setQuery={setQuery}
              selected={selected}
              setSelected={setSelected}
              onNavigate={onNavigate}
              onNavigating={() => {
                navigating.current = true;
              }}
            />
          ) : failed ? (
            <div role="alert">
              <p>{t("common.loadFailed")}</p>
              <Button
                data-testid="help-retry"
                onClick={() => setAttempt((value) => value + 1)}
              >
                {t("common.retry")}
              </Button>
            </div>
          ) : (
            <p role="status">{t("common.loading")}</p>
          ))}
      </DialogContent>
    </Dialog>
  );
}
