import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy } from "lucide-react";
import type { CodeBlock } from "@/lib/codeBlock";
import { cn } from "@/lib/utils";

/** Literal, bounded rendering; clipboard authority stays with the message owner. */
export function CodeBlockView({
  block,
  onCopy,
}: {
  block: CodeBlock;
  onCopy?: () => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  return (
    <div
      className="min-w-0 overflow-hidden rounded-md border border-current/15 bg-background/60 text-foreground"
      data-testid="message-code-block"
    >
      <div className="flex items-center justify-between gap-4 border-b border-current/10 px-3 py-1.5 text-xs">
        <span className="truncate font-mono">
          {block.language || t("codeBlock.plainText")}
        </span>
        {onCopy && (
          <button
            type="button"
            data-testid="code-copy"
            onClick={onCopy}
            aria-label={t("codeBlock.copy")}
            className="flex shrink-0 items-center gap-1 rounded px-2 py-1 hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
          >
            <Copy className="h-3 w-3" />
            {t("codeBlock.copy")}
          </button>
        )}
      </div>
      <pre
        tabIndex={0}
        aria-label={t("codeBlock.compose")}
        className={cn(
          "m-0 overflow-auto whitespace-pre p-3 font-mono text-[13px] leading-5 [tab-size:4]",
          expanded ? "max-h-[70vh]" : "max-h-64",
        )}
      >
        <code>{block.code}</code>
      </pre>
      {block.code.split("\n").length > 12 && (
        <button
          type="button"
          data-testid="code-expand"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
          className="w-full border-t border-current/10 px-3 py-1.5 text-xs hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t(expanded ? "codeBlock.collapse" : "codeBlock.expand")}
        </button>
      )}
    </div>
  );
}
