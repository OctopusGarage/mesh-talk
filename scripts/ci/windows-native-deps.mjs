// The MSVC OpenSSL build needs native Perl, not Git Bash's partial MSYS Perl.
import { spawnSync } from "node:child_process";
import { appendFileSync, existsSync } from "node:fs";
import { win32 } from "node:path";
import { fileURLToPath } from "node:url";

export const perlProbe =
  'use strict; use warnings; use Config; use IPC::Cmd; use Locale::Maketext::Simple; die "Native Windows Perl required\\n" unless $^O eq "MSWin32"; print "mesh-talk-native-perl-ok\\n";';

export function selectNativePerl({
  env = process.env,
  exists = existsSync,
  run = spawnSync,
} = {}) {
  const explicit = env.OPENSSL_SRC_PERL || env.PERL;
  const searchPath =
    Object.entries(env).find(([key]) => key.toLowerCase() === "path")?.[1] ??
    "";
  const candidates = explicit
    ? [explicit]
    : [
        ...searchPath
          .split(";")
          .filter(Boolean)
          .map((directory) =>
            win32.join(directory.replace(/^"|"$/gu, ""), "perl.exe"),
          ),
        "C:\\Strawberry\\perl\\bin\\perl.exe",
      ];
  const checked = new Set();
  const failures = [];
  for (const candidate of candidates) {
    if (!win32.isAbsolute(candidate) || /[\r\n\0]/u.test(candidate)) {
      if (explicit)
        throw Error(
          "Explicit Perl must be an absolute Windows executable path",
        );
      continue;
    }
    const filename = win32.normalize(candidate);
    if (checked.has(filename.toLowerCase())) continue;
    checked.add(filename.toLowerCase());
    if (!exists(filename)) continue;
    const result = run(filename, ["-e", perlProbe], {
      env,
      encoding: "utf8",
      shell: false,
      windowsHide: true,
      timeout: 10_000,
      maxBuffer: 64 * 1024,
    });
    if (
      !result.error &&
      result.status === 0 &&
      result.stdout?.trim() === "mesh-talk-native-perl-ok"
    )
      return filename;
    failures.push(
      `${filename}: ${result.error?.message ?? result.stderr?.trim() ?? `exit ${result.status}`}`,
    );
  }
  throw Error(
    `Native Windows Perl with IPC::Cmd and Locale::Maketext::Simple is required. Install Strawberry Perl or set OPENSSL_SRC_PERL to a complete native installation.\n${failures.join("\n")}`,
  );
}

export function prepareWindowsNativeDependencies({
  platform = process.platform,
  env = process.env,
  append = appendFileSync,
  ...probe
} = {}) {
  if (platform !== "win32")
    throw Error("Windows native dependency preparation requires Windows");
  if (!env.GITHUB_ENV)
    throw Error(
      "GITHUB_ENV is required to pin Perl for subsequent Cargo steps",
    );
  const perl = selectNativePerl({ env, ...probe });
  // openssl-src prefers OPENSSL_SRC_PERL; PERL covers its subprocesses and older versions.
  append(env.GITHUB_ENV, `OPENSSL_SRC_PERL=${perl}\nPERL=${perl}\n`, "utf8");
  return perl;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    console.log(
      `Validated native OpenSSL Perl: ${prepareWindowsNativeDependencies()}`,
    );
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
