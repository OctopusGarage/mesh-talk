import type { CSSProperties, Dispatch, Ref, SetStateAction } from "react";
import {
  ArrowUpRight,
  Check,
  Circle,
  Crop,
  Grid2X2,
  Pencil,
  RotateCcw,
  Square,
  Type,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { colors, type Ink, type Tool } from "./annotations";

const buttonClass =
  "flex h-9 w-9 shrink-0 items-center justify-center rounded hover:bg-black/5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-600 disabled:opacity-35";

export function ScreenshotToolbar({
  ref,
  position,
  selected,
  sending,
  tool,
  setTool,
  ink,
  setInk,
  label,
  setLabel,
  canUndo,
  onUndo,
  onCancel,
  onConfirm,
  failure,
}: {
  ref: Ref<HTMLDivElement>;
  position: CSSProperties;
  selected: boolean;
  sending: boolean;
  tool: Tool;
  setTool: (tool: Tool) => void;
  ink: Ink;
  setInk: Dispatch<SetStateAction<Ink>>;
  label: string;
  setLabel: (label: string) => void;
  canUndo: boolean;
  onUndo: () => void;
  onCancel: () => void;
  onConfirm: () => void;
  failure: string | null;
}) {
  const { t } = useTranslation();
  return (
    <div
      ref={ref}
      data-testid="screenshot-toolbar"
      className="absolute flex w-max max-w-[calc(100vw-16px)] flex-col gap-1 rounded-md bg-white p-1.5 shadow-[0_4px_20px_rgba(0,0,0,0.28)]"
      style={position}
    >
      <div
        role="group"
        aria-label={t("screenshot.editorTitle")}
        className="flex flex-wrap items-center gap-0.5"
      >
        {(
          [
            ["select", <Crop size={18} />, "selectRegion"],
            ["rect", <Square size={18} />, "rectangle"],
            ["ellipse", <Circle size={18} />, "ellipse"],
            ["arrow", <ArrowUpRight size={18} />, "arrow"],
            ["pen", <Pencil size={18} />, "pen"],
            ["mosaic", <Grid2X2 size={18} />, "mosaic"],
            ["text", <Type size={18} />, "text"],
          ] as const
        ).map(([value, icon, key]) => (
          <button
            key={value}
            type="button"
            title={t(`screenshot.${key}`)}
            aria-label={t(`screenshot.${key}`)}
            aria-pressed={tool === value}
            disabled={sending || (!selected && value !== "select")}
            onClick={() => setTool(value)}
            className={`${buttonClass} ${tool === value ? "bg-emerald-50 text-emerald-700" : ""}`}
          >
            {icon}
          </button>
        ))}
        <span className="mx-1 h-5 w-px bg-neutral-200" />
        <button
          type="button"
          title={t("screenshot.undo")}
          aria-label={t("screenshot.undo")}
          disabled={!canUndo || sending}
          onClick={onUndo}
          className={buttonClass}
        >
          <RotateCcw size={18} />
        </button>
        <button
          type="button"
          title={t("screenshot.cancel")}
          aria-label={t("screenshot.cancel")}
          disabled={sending}
          onClick={onCancel}
          className={buttonClass}
        >
          <X size={19} />
        </button>
        <button
          type="button"
          data-testid="screenshot-send"
          title={t("screenshot.confirm")}
          aria-label={t("screenshot.confirm")}
          disabled={!selected || sending}
          onClick={onConfirm}
          className={`${buttonClass} text-emerald-700`}
        >
          <Check size={21} />
        </button>
      </div>
      {tool !== "select" && (
        <div className="flex flex-wrap items-center gap-2 border-t border-neutral-100 px-1 py-1">
          {tool !== "mosaic" && (
            <div
              role="group"
              aria-label={t("screenshot.color")}
              className="flex gap-1"
            >
              {colors.map((color) => (
                <button
                  key={color}
                  type="button"
                  aria-label={t("screenshot.colorValue", { color })}
                  aria-pressed={ink.color === color}
                  disabled={sending}
                  onClick={() => setInk((value) => ({ ...value, color }))}
                  className="flex h-7 w-7 items-center justify-center rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-600"
                >
                  <span
                    className={`h-4 w-4 rounded-full border border-black/20 ${ink.color === color ? "ring-2 ring-neutral-700 ring-offset-2" : ""}`}
                    style={{ background: color }}
                  />
                </button>
              ))}
            </div>
          )}
          <div
            role="group"
            aria-label={t("screenshot.strokeWidth")}
            className="flex gap-0.5"
          >
            {[2, 3, 6].map((width) => (
              <button
                key={width}
                type="button"
                aria-label={t("screenshot.strokeValue", { width })}
                aria-pressed={ink.width === width}
                disabled={sending}
                onClick={() => setInk((value) => ({ ...value, width }))}
                className={`flex h-7 w-7 items-center justify-center rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-emerald-600 ${ink.width === width ? "bg-neutral-200" : "hover:bg-neutral-100"}`}
              >
                <span
                  className="rounded-full bg-neutral-700"
                  style={{ width: width + 2, height: width + 2 }}
                />
              </button>
            ))}
          </div>
          {tool === "text" && (
            <input
              type="text"
              value={label}
              disabled={sending}
              onChange={(event) => setLabel(event.target.value)}
              maxLength={200}
              aria-label={t("screenshot.textValue")}
              placeholder={t("screenshot.textValue")}
              className="h-8 w-36 rounded border border-neutral-300 px-2 text-sm outline-none focus:border-emerald-600"
            />
          )}
        </div>
      )}
      {failure && (
        <p role="alert" className="max-w-sm px-2 py-1 text-sm text-red-700">
          {failure}
        </p>
      )}
    </div>
  );
}
