import { expect, it } from "vitest";
import { clipboardFiles } from "./clipboardFiles";

it("prefers copied files over the filename text also supplied by the clipboard", () => {
  const file = new File(["hello"], "report.txt", { type: "text/plain" });
  const data = { files: [file], items: [], getData: () => "report.txt" };
  expect(clipboardFiles(data)).toEqual([file]);
});

it("falls back to image clipboard items when files is empty", () => {
  const image = new File(["png"], "image.png", { type: "image/png" });
  const data = {
    files: [],
    items: [{ kind: "file", type: "image/png", getAsFile: () => image }],
  };
  expect(clipboardFiles(data)).toEqual([image]);
});

it("leaves ordinary text paste alone", () => {
  expect(clipboardFiles({ files: [], items: [] })).toEqual([]);
});

it("keeps empty files as attachments", () => {
  const file = new File([], "empty.txt");
  expect(clipboardFiles({ files: [file], items: [] })).toEqual([file]);
});
