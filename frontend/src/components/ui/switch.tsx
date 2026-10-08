import { cn } from "@/lib/utils";

interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  id?: string;
  "aria-label"?: string;
}

/**
 * A minimal toggle switch (no @radix-ui/react-switch dep). Controlled: the parent
 * owns `checked` and reacts to `onCheckedChange`.
 */
export function Switch({
  checked,
  onCheckedChange,
  disabled,
  id,
  ...rest
}: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      id={id}
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onCheckedChange(!checked)}
      className={cn(
        "inline-flex h-9 w-10 shrink-0 cursor-pointer items-center justify-center rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11",
        "disabled:cursor-not-allowed disabled:opacity-50",
      )}
      {...rest}
    >
      <span
        className={cn(
          "pointer-events-none relative flex h-5 w-9 items-center rounded-full transition-colors",
          checked ? "bg-primary" : "bg-input",
        )}
      >
        <span
          className={cn(
            "block h-4 w-4 rounded-full bg-background shadow transition-transform",
            checked ? "translate-x-[18px]" : "translate-x-0.5",
          )}
        />
      </span>
    </button>
  );
}
