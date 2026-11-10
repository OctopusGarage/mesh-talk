import { useEffect, useState } from "react";

/** Local preview only; the composer retains the File until explicit send or dismissal. */
export function ComposerImagePreview({ file }: { file: File }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    const preview = URL.createObjectURL(file);
    setUrl(preview);
    return () => URL.revokeObjectURL(preview);
  }, [file]);
  return (
    <img
      data-testid="composer-pending-image"
      src={url ?? undefined}
      alt=""
      aria-hidden="true"
      className="h-20 w-40 shrink-0 rounded object-contain"
    />
  );
}
