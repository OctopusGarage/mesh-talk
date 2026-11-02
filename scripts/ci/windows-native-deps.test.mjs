import assert from "node:assert/strict";
import test from "node:test";
import { selectNativePerl } from "./windows-native-deps.mjs";

test(
  "the actual Windows runner provides a complete native Perl installation",
  {
    skip:
      process.platform === "win32"
        ? false
        : "Requires the native Windows interpreter",
  },
  () => {
    assert.match(selectNativePerl(), /[\\/]perl\.exe$/iu);
  },
);
