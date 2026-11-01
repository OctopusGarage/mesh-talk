import * as React from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";
import { cn } from "@/lib/utils";

const Tabs = TabsPrimitive.Root;

const TabsList = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.List>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>
>(({ className, children, ...props }, ref) => {
  const host = React.useRef<HTMLDivElement | null>(null);
  const [indicator, setIndicator] = React.useState<React.CSSProperties>();
  const [pointer, setPointer] = React.useState(false);
  React.useEffect(() => {
    const list = host.current;
    if (!list) return;
    const measure = () => {
      const active = list.querySelector<HTMLElement>(
        '[role="tab"][data-state="active"]',
      );
      setIndicator(
        active && active.offsetWidth > 0
          ? {
              width: active.offsetWidth,
              height: active.offsetHeight,
              transform: `translate(${active.offsetLeft}px, ${active.offsetTop}px)`,
            }
          : undefined,
      );
    };
    const resize = new ResizeObserver(measure);
    const observeGeometry = () => {
      resize.disconnect();
      resize.observe(list);
      list
        .querySelectorAll<HTMLElement>('[role="tab"]')
        .forEach((tab) => resize.observe(tab));
      measure();
    };
    const observer = new MutationObserver((records) => {
      if (
        records.some(
          (record) =>
            !(record.target instanceof Element) ||
            !record.target.classList.contains("tab-indicator"),
        )
      )
        observeGeometry();
    });
    observer.observe(list, {
      subtree: true,
      attributes: true,
      attributeFilter: ["data-state", "class", "style", "hidden"],
      childList: true,
      characterData: true,
    });
    observeGeometry();
    return () => {
      observer.disconnect();
      resize.disconnect();
    };
  }, []);
  return (
    <TabsPrimitive.List
      ref={(node) => {
        host.current = node;
        if (typeof ref === "function") ref(node);
        else if (ref) ref.current = node;
      }}
      className={cn(
        "motion-tabs relative isolate inline-flex h-9 items-center justify-center rounded-md bg-muted p-0.5 text-muted-foreground",
        className,
      )}
      data-pointer-motion={pointer || undefined}
      data-indicator={indicator ? "ready" : undefined}
      {...props}
      onPointerDownCapture={(event) => {
        setPointer(true);
        props.onPointerDownCapture?.(event);
      }}
      onKeyDownCapture={(event) => {
        setPointer(false);
        props.onKeyDownCapture?.(event);
      }}
    >
      {indicator && (
        <span
          aria-hidden="true"
          className="tab-indicator pointer-events-none absolute left-0 top-0 -z-10 rounded bg-background shadow-sm"
          style={indicator}
        />
      )}
      {children}
    </TabsPrimitive.List>
  );
});
TabsList.displayName = TabsPrimitive.List.displayName;

const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Trigger
    ref={ref}
    className={cn(
      "inline-flex items-center justify-center whitespace-nowrap rounded px-3 py-1.5 text-[13px] font-medium transition-[background-color,color,box-shadow] duration-150 ease-out disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-sm",
      className,
    )}
    {...props}
  />
));
TabsTrigger.displayName = TabsPrimitive.Trigger.displayName;

const TabsContent = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Content
    ref={ref}
    className={cn(
      "mt-2 ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
      className,
    )}
    {...props}
  />
));
TabsContent.displayName = TabsPrimitive.Content.displayName;

export { Tabs, TabsList, TabsTrigger, TabsContent };
