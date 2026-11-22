import jsQR from "jsqr";

self.onmessage = ({
  data,
}: MessageEvent<{
  pixels: Uint8ClampedArray;
  width: number;
  height: number;
}>) => {
  const result = jsQR(data.pixels, data.width, data.height);
  self.postMessage(result?.data ?? null);
};
