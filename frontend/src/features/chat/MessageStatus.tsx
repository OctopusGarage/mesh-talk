import {
  useId,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
  type KeyboardEvent,
} from "react";

/** A quiet status signal with explicit, in-flow details beneath its message. */
export function MessageStatus({
  label,
  icon,
  count,
  delivery,
  custody,
  testId = "message-status",
  heading = label,
  children,
}: {
  label: string;
  icon: ReactNode;
  count?: string;
  delivery?: string;
  custody?: number | "unknown";
  testId?: string;
  heading?: string;
  children: (controls: {
    close: () => void;
    trigger: RefObject<HTMLButtonElement | null>;
  }) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const id = useId();
  const close = () => setOpen(false);
  const escape = (event: KeyboardEvent) => {
    if (event.key !== "Escape" || !open) return;
    event.preventDefault();
    event.stopPropagation();
    close();
    trigger.current?.focus();
  };
  return (
    <>
      <button
        ref={trigger}
        type="button"
        data-testid={`${testId}-trigger`}
        data-delivery={delivery}
        data-custody={custody}
        aria-label={label}
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={escape}
        className="inline-flex min-h-6 min-w-6 shrink-0 items-center justify-center gap-0.5 rounded-sm px-1 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        {icon}
        {count && (
          <span data-status-count aria-hidden="true">
            {count}
          </span>
        )}
        <span className="sr-only">{label}</span>
      </button>
      {/* Reserve the scroll track before WebKit measures the wrapped flex row. */}
      {open && (
        <div
          id={id}
          data-testid={testId}
          role="group"
          aria-labelledby={`${id}-heading`}
          aria-describedby={`${id}-description`}
          tabIndex={0}
          onKeyDown={escape}
          className="max-h-[min(16rem,40vh)] min-w-0 basis-full overflow-y-scroll pt-1 text-left text-xs leading-relaxed outline-offset-2 focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring [overflow-wrap:anywhere]"
        >
          <div className="ml-auto max-w-sm space-y-2">
            <h3 id={`${id}-heading`} className="font-medium text-foreground">
              {heading}
            </h3>
            <div id={`${id}-description`} className="space-y-2">
              {children({ close, trigger })}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
