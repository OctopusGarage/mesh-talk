import assert from "node:assert/strict";
import { inflateRawSync } from "node:zlib";
import { crc32 } from "./png-evidence.mjs";

// Bounded ZIP32 reader for Playwright traces, not an archive extraction tool.
export function inspectTrace(bytes) {
  assert.ok(Buffer.isBuffer(bytes) && bytes.length >= 22 && bytes.length <= 64 * 1024 * 1024, "Trace ZIP size limit");
  let end = -1;
  for (let offset = bytes.length - 22; offset >= Math.max(0, bytes.length - 65557); offset--) {
    if (bytes.readUInt32LE(offset) === 0x06054b50 && offset + 22 + bytes.readUInt16LE(offset + 20) === bytes.length) { end = offset; break; }
  }
  assert.ok(end >= 0, "Missing trace ZIP central directory ending");
  const count = bytes.readUInt16LE(end + 10), size = bytes.readUInt32LE(end + 12), start = bytes.readUInt32LE(end + 16);
  assert.ok(bytes.readUInt16LE(end + 4) === 0 && bytes.readUInt16LE(end + 6) === 0 && bytes.readUInt16LE(end + 8) === count && count > 0 && count < 10000 && start + size === end, "Invalid or unsupported ZIP directory");
  const names = new Set(), ranges = [];
  let offset = start, decoded = 0, traceEntries = 0, actionRecords = 0, before = false, after = false;
  for (let entry = 0; entry < count; entry++) {
    assert.ok(offset + 46 <= end && bytes.readUInt32LE(offset) === 0x02014b50, "Truncated ZIP entry");
    const flags = bytes.readUInt16LE(offset + 8), method = bytes.readUInt16LE(offset + 10), checksum = bytes.readUInt32LE(offset + 16);
    const compressed = bytes.readUInt32LE(offset + 20), unpacked = bytes.readUInt32LE(offset + 24);
    const nameLength = bytes.readUInt16LE(offset + 28), extra = bytes.readUInt16LE(offset + 30), comment = bytes.readUInt16LE(offset + 32), local = bytes.readUInt32LE(offset + 42);
    assert.ok(!(flags & 1) && [0, 8].includes(method) && bytes.readUInt16LE(offset + 34) === 0 && unpacked <= 64 * 1024 * 1024 && compressed <= bytes.length, "Unsupported or oversized ZIP entry");
    const next = offset + 46 + nameLength + extra + comment;
    assert.ok(next <= end && nameLength > 0, "Invalid ZIP entry name");
    const nameBytes = bytes.subarray(offset + 46, offset + 46 + nameLength), name = nameBytes.toString("utf8");
    assert.ok(!names.has(name) && !name.startsWith("/") && !name.includes("\\") && !name.split("/").includes(".."), "Unsafe or duplicate trace ZIP name");
    names.add(name);
    assert.ok(local + 30 <= start && bytes.readUInt32LE(local) === 0x04034b50, "Invalid ZIP local header");
    assert.equal(bytes.readUInt16LE(local + 6), flags, "ZIP flags mismatch");
    assert.equal(bytes.readUInt16LE(local + 8), method, "ZIP compression mismatch");
    const localNameLength = bytes.readUInt16LE(local + 26), localExtra = bytes.readUInt16LE(local + 28);
    assert.equal(localNameLength, nameLength, "ZIP name length mismatch");
    const dataStart = local + 30 + localNameLength + localExtra, dataEnd = dataStart + compressed;
    assert.ok(dataEnd <= start, "Truncated or overlapping ZIP payload");
    assert.ok(bytes.subarray(local + 30, local + 30 + nameLength).equals(nameBytes), "ZIP filename mismatch");
    decoded += unpacked;
    assert.ok(decoded <= 128 * 1024 * 1024, "Trace ZIP decoded size limit");
    const packed = bytes.subarray(dataStart, dataEnd);
    const data = method === 8 ? inflateRawSync(packed, { maxOutputLength: Math.max(1, unpacked) }) : packed;
    assert.equal(data.length, unpacked, "Incomplete ZIP decoded payload");
    assert.equal(crc32(data), checksum, "Corrupt ZIP payload CRC");
    let recordEnd = dataEnd;
    if (flags & 8) {
      const descriptor = dataEnd + (dataEnd + 4 <= start && bytes.readUInt32LE(dataEnd) === 0x08074b50 ? 4 : 0);
      assert.ok(descriptor + 12 <= start, "Missing ZIP data descriptor");
      assert.equal(bytes.readUInt32LE(descriptor), checksum, "ZIP descriptor CRC mismatch");
      assert.equal(bytes.readUInt32LE(descriptor + 4), compressed, "ZIP descriptor length mismatch");
      assert.equal(bytes.readUInt32LE(descriptor + 8), unpacked, "ZIP descriptor decoded length mismatch");
      recordEnd = descriptor + 12;
    } else {
      assert.equal(bytes.readUInt32LE(local + 14), checksum, "ZIP local CRC mismatch");
      assert.equal(bytes.readUInt32LE(local + 18), compressed, "ZIP local length mismatch");
      assert.equal(bytes.readUInt32LE(local + 22), unpacked, "ZIP local decoded length mismatch");
    }
    ranges.push([local, recordEnd]);
    if (name.endsWith(".trace")) {
      traceEntries++;
      for (const line of data.toString("utf8").split("\n").filter(Boolean)) {
        const record = JSON.parse(line);
        assert.equal(typeof record.type, "string", "Invalid Playwright trace record");
        if (["before", "after"].includes(record.type)) actionRecords++;
        before ||= record.type === "before"; after ||= record.type === "after";
      }
    }
    offset = next;
  }
  assert.equal(offset, end, "Trace ZIP directory length mismatch");
  ranges.sort((a, b) => a[0] - b[0]);
  let cursor = 0;
  for (const [local, recordEnd] of ranges) { assert.equal(local, cursor, "Trace ZIP overlapping entries or unexplained data"); cursor = recordEnd; }
  assert.equal(cursor, start, "Trace ZIP payload directory boundary mismatch");
  assert.ok(traceEntries > 0 && before && after, "Trace ZIP contains no completed Playwright actions");
  return { entries: count, traceEntries, actionRecords };
}
