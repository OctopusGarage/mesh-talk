import { useEffect, useState } from "react";

/** Local preview only; the composer retains the File until explicit send or dismissal. */
export function ComposerImagePreview({ file }: { file: File }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    const preview = URL.createObjectURL(file);
    setUrl(preview);
    return () => URL.revokeObjectURL(preview);
  }, [file]);
  return url ? (
    <img
      data-testid="composer-pending-image"
      src={url}
      alt=""
      aria-hidden="true"
      className="h-20 max-w-40 rounded object-contain"
    />
  ) : null;
}
