import { useLayoutEffect, useRef, useState } from "react";
import "@fontsource-variable/inter";
import "@fontsource-variable/space-grotesk";
import "@fontsource-variable/geist-mono";
import { useDemo } from "./model";
import { Overlay } from "./parts";
import { Native } from "./Native";
import { Technical } from "./Technical";
import { Premium } from "./Premium";
import "./styles.css";
import "./picker.css";

const names = ["Native", "Technical", "Understated"];
const initial = Math.max(
  0,
  Math.min(2, (Number(new URLSearchParams(location.search).get("v")) || 1) - 1),
);

function Variant({ index }: { index: number }) {
  const demo = useDemo();
  return (
    <div className="prototype-stage">
      {index === 0 ? (
        <Native demo={demo} />
      ) : index === 1 ? (
        <Technical demo={demo} />
      ) : (
        <Premium demo={demo} />
      )}
      {demo.notice && (
        <div className="demo-notice" role="status">
          {demo.notice}
          <button
            aria-label="Dismiss notice"
            onClick={() => demo.setNotice("")}
          >
            ×
          </button>
        </div>
      )}
      <Overlay demo={demo} />
    </div>
  );
}

export function Harness() {
  const [index, setIndex] = useState(initial);
  const [revision, setRevision] = useState(0);
  const picker = useRef<HTMLElement>(null);
  const highlight = useRef<HTMLSpanElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);
  const choose = (next: number) => {
    if (next < 0 || next >= names.length) return;
    setIndex(next);
    setRevision((n) => n + 1);
    const url = new URL(location.href);
    url.searchParams.set("v", String(next + 1));
    history.replaceState(null, "", url);
  };
  const replay = () => setRevision((n) => n + 1);
  useLayoutEffect(() => {
    const moveHighlight = () => {
      const el = items.current[index];
      if (!el || !highlight.current) return;
      highlight.current.style.width = `${el.offsetWidth}px`;
      highlight.current.style.transform = `translateX(${el.offsetLeft}px)`;
    };
    moveHighlight();
    window.addEventListener("resize", moveHighlight);
    return () => window.removeEventListener("resize", moveHighlight);
  }, [index]);
  useLayoutEffect(() => {
    const id = requestAnimationFrame(() =>
      requestAnimationFrame(() =>
        picker.current?.setAttribute("data-ready", ""),
      ),
    );
    return () => cancelAnimationFrame(id);
  }, []);
  useLayoutEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (
        /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) ||
        el.isContentEditable ||
        e.metaKey ||
        e.ctrlKey ||
        e.altKey
      )
        return;
      const num = parseInt(e.key, 10);
      if (num >= 1 && num <= names.length) choose(num - 1);
      else if (e.key === "ArrowRight") choose((index + 1) % names.length);
      else if (e.key === "ArrowLeft")
        choose((index - 1 + names.length) % names.length);
      else if (e.key.toLowerCase() === "r") replay();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [index]);
  return (
    <>
      <Variant key={`${index}-${revision}`} index={index} />
      <nav
        ref={picker}
        className="proto-picker"
        data-position="top"
        aria-label="Prototype variants"
      >
        <span
          ref={highlight}
          className="proto-picker-highlight"
          aria-hidden="true"
        />
        {names.map((name, i) => (
          <button
            key={name}
            ref={(el) => {
              items.current[i] = el;
            }}
            className="proto-picker-item"
            data-active={index === i ? "" : undefined}
            aria-current={index === i ? "true" : undefined}
            onClick={() => choose(i)}
          >
            {name}
          </button>
        ))}
        <span className="proto-picker-divider" aria-hidden="true" />
        <button
          className="proto-picker-item proto-picker-replay"
          aria-label="Replay animation (R)"
          onClick={replay}
        >
          ↻
        </button>
      </nav>
    </>
  );
}
