/** File entries take precedence over the filename text that desktop clipboards also carry. */
export function clipboardFiles(data: {
  files: ArrayLike<File>;
  items: ArrayLike<Pick<DataTransferItem, "kind" | "getAsFile">>;
}): File[] {
  const files = Array.from(data.files).filter((file) => file.name.length > 0);
  if (files.length) return files;
  return Array.from(data.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null && file.name.length > 0);
}
