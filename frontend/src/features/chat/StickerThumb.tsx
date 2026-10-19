import { useEffect, useState } from "react";
import { useMotionOK } from "@/lib/motion";

/** Render a decoded still frame when motion is reduced or the preference is unknown. */
export function StickerThumb({
  src,
  alt,
  fallback,
}: {
  src: string;
  alt: string;
  fallback: string;
}) {
  const motionOK = useMotionOK();
  const [thumb, setThumb] = useState<{ src: string; url: string } | null>(null);

  useEffect(() => {
    if (motionOK || !src) return;
    let active = true;
    setThumb(null);

    void (async () => {
      try {
        const blob = await fetch(src).then((res) => res.blob());
        const bitmap = await createImageBitmap(blob);
        try {
          if (!active) return;
          const canvas = document.createElement("canvas");
          canvas.width = bitmap.width;
          canvas.height = bitmap.height;
          const ctx = canvas.getContext("2d");
          if (!ctx) return;
          ctx.drawImage(bitmap, 0, 0);
          const url = canvas.toDataURL("image/webp");
          if (active) setThumb({ src, url });
        } finally {
          bitmap.close();
        }
      } catch {
        if (active) setThumb(null);
      }
    })();

    return () => {
      active = false;
    };
  }, [motionOK, src]);

  const image = motionOK ? src : thumb?.src === src ? thumb.url : null;
  return src && image ? (
    <img
      src={image}
      alt={alt}
      loading={motionOK ? "lazy" : "eager"}
      decoding="async"
      width={48}
      height={48}
      className="h-12 w-12"
      draggable={false}
    />
  ) : (
    <div
      className="flex h-12 w-12 items-center justify-center text-2xl leading-none"
      aria-hidden="true"
    >
      {fallback}
    </div>
  );
}
