import { createElement } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { useFileObjectUrl } from "../src/features/chat/mediaFile";
import { useAuth } from "../src/store/auth";

/** Browser-only real hook harness; no jsdom or mocked React lifecycle. */
export function mountMediaProbe(container: HTMLElement) {
  const root = createRoot(container);
  function Probe({ enabled }: { enabled: boolean }) {
    const url = useFileObjectUrl("hook-probe", enabled, "image/png");
    return <span data-testid="media-hook-url" data-url={url ?? ""} />;
  }
  return {
    enable: (enabled: boolean) =>
      flushSync(() => root.render(createElement(Probe, { enabled }))),
    dispose: () => flushSync(() => root.unmount()),
    relogin: async () => {
      await useAuth.getState().logout();
      await useAuth.getState().login("tester", "password123");
    },
    logout: () => useAuth.getState().logout(),
  };
}
