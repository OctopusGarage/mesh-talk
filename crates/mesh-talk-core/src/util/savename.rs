//! Safe handling of a received file's (remote-supplied) name when turning it into a
//! save path. Remote-controlled filenames are an injection surface: a peer could send
//! a `name` of `../../.ssh/authorized_keys`, an absolute path, a Windows drive
//! (`C:\…`), or characters illegal on the local filesystem. Two concerns:
//!
//! 1. [`sanitize_filename`] reduces an arbitrary remote name to a single, legal path
//!    component (no directory parts, no traversal, OS-illegal chars replaced).
//! 2. [`safe_save_path`] joins the sanitized name under a chosen directory and
//!    verifies the result stays within that directory, de-duplicating with a counter
//!    suffix so two saves never clobber.

use std::path::{Component, Path, PathBuf};

/// Characters that are illegal in a filename on at least one supported OS (Windows is
/// the strict one), plus the path separators. Replaced with `_`.
fn is_illegal_char(c: char) -> bool {
    matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 0x20
    // control chars
}

/// Reduce a remote-supplied name to a single safe path component.
///
/// - strips any directory components (keeps only the final segment),
/// - rejects `.`/`..`/empty by falling back to `"file"`,
/// - replaces OS-illegal characters (and control chars) with `_`,
/// - trims trailing dots/spaces (illegal/awkward on Windows),
/// - caps length so a pathological name can't blow the OS path limit.
///
/// The result is always a non-empty, single-segment, legal filename.
pub fn sanitize_filename(raw: &str) -> String {
    // Take the last segment after either separator — handles both `a/b` and `a\b`,
    // and absolute / drive-prefixed paths (`/x`, `C:\x`) collapse to the tail.
    let tail = raw.rsplit(['/', '\\']).next().unwrap_or("").trim();

    // `.` / `..` (or empty) are not real names.
    if tail.is_empty() || tail == "." || tail == ".." {
        return "file".to_string();
    }

    let mut cleaned: String = tail
        .chars()
        .map(|c| if is_illegal_char(c) { '_' } else { c })
        .collect();

    // Windows: trailing dots and spaces are stripped by the OS, making the effective
    // name differ from what we checked. Trim them ourselves.
    let trimmed = cleaned.trim_end_matches([' ', '.']);
    if trimmed.len() != cleaned.len() {
        cleaned = trimmed.to_string();
    }
    if cleaned.is_empty() {
        return "file".to_string();
    }

    // Cap to a sane length (bytes), preserving the extension where possible.
    const MAX: usize = 200;
    if cleaned.len() > MAX {
        cleaned = truncate_keeping_ext(&cleaned, MAX);
    }
    cleaned
}

/// Truncate to at most `max` bytes on a char boundary, keeping the extension if one
/// fits. Used only for pathologically long names.
fn truncate_keeping_ext(name: &str, max: usize) -> String {
    let (stem, ext) = split_ext(name);
    // `.ext` plus at least one stem char, else just hard-truncate the whole thing.
    if !ext.is_empty() && ext.len() + 1 < max {
        let keep = max - ext.len();
        let mut s = floor_char_boundary(stem, keep).to_string();
        s.push_str(ext);
        s
    } else {
        floor_char_boundary(name, max).to_string()
    }
}

/// Largest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn floor_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Split a filename into `(stem, ext)` where `ext` INCLUDES the leading dot (or is
/// empty). A leading-dot name (`.bashrc`) has no extension. Mirrors the insertion
/// point [`safe_save_path`] uses for its ` (N)` counter.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        // A dot at index 0 is a hidden-file prefix, not an extension separator.
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Insert / bump a ` (N)` counter suffix BEFORE the extension. The first collision
/// yields `name (1).ext`; an existing ` (N)` is REUSED (bumped) rather than stacked,
/// so we never produce `name (1) (1).ext`.
fn with_counter(name: &str, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    // Reuse an existing trailing " (\d+)" on the stem instead of appending a new one.
    let base = strip_counter(stem);
    format!("{base} ({n}){ext}")
}

/// Strip a trailing ` (\d+)` from a stem, if present (the inverse of [`with_counter`]'s
/// suffix). Implemented without a regex dependency.
fn strip_counter(stem: &str) -> &str {
    let bytes = stem.as_bytes();
    if bytes.last() != Some(&b')') {
        return stem;
    }
    // Walk back over the digits.
    let mut i = stem.len() - 1; // at ')'
    let mut j = i; // will land just past the '('
    let mut saw_digit = false;
    // step before ')'
    if i == 0 {
        return stem;
    }
    i -= 1;
    while i > 0 && bytes[i].is_ascii_digit() {
        saw_digit = true;
        i -= 1;
    }
    // need at least one digit, a '(' at i, and a space before it
    if saw_digit && bytes[i] == b'(' && i >= 1 && bytes[i - 1] == b' ' {
        j = i - 1; // index of the space
        return &stem[..j];
    }
    let _ = j;
    stem
}

/// Turn a remote-supplied `raw_name` into a concrete, safe save path under `dir`,
/// de-duplicating against existing files with a counter suffix.
///
/// Guarantees:
/// - the name is sanitized to a single legal component (see [`sanitize_filename`]),
/// - the final path is confirmed to stay within `dir` (defence in depth — sanitize
///   already removes traversal, but we re-check the joined path's components),
/// - if `name.ext` already exists, returns `name (1).ext`, `name (2).ext`, … reusing
///   an existing ` (N)` rather than stacking suffixes.
///
/// Returns `None` only if, against expectation, the joined path escapes `dir`.
pub fn safe_save_path(dir: &Path, raw_name: &str) -> Option<PathBuf> {
    let name = sanitize_filename(raw_name);
    let candidate = dir.join(&name);
    if !is_within(dir, &candidate) {
        return None;
    }
    if !candidate.exists() {
        return Some(candidate);
    }
    for n in 1..=u32::MAX {
        let alt = dir.join(with_counter(&name, n));
        if !is_within(dir, &alt) {
            return None;
        }
        if !alt.exists() {
            return Some(alt);
        }
    }
    None
}

/// True if `path` is `dir` itself or strictly inside it, judged lexically on normal
/// path components (no `..`, no root re-anchor). The sanitized name is a single
/// `Normal` component, so this is belt-and-braces.
fn is_within(dir: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(dir) else {
        return false;
    };
    rel.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_directory_components() {
        assert_eq!(sanitize_filename("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_filename("a\\b\\c.txt"), "c.txt");
        assert_eq!(sanitize_filename("/etc/passwd"), "passwd");
        assert_eq!(
            sanitize_filename("C:\\Windows\\system32\\evil.dll"),
            "evil.dll"
        );
    }

    #[test]
    fn rejects_traversal_and_dot_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename(".."), "file");
        assert_eq!(sanitize_filename("."), "file");
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(sanitize_filename("../"), "file"); // tail after split is empty
    }

    #[test]
    fn replaces_illegal_chars() {
        assert_eq!(sanitize_filename("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_filename("a<b>c|d\"e.txt"), "a_b_c_d_e.txt");
        // a control char becomes _
        assert_eq!(sanitize_filename("a\u{7}b.txt"), "a_b.txt");
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_filename("name.   "), "name");
        assert_eq!(sanitize_filename("name..."), "name");
        assert_eq!(sanitize_filename("name . "), "name");
    }

    #[test]
    fn keeps_normal_names_and_hidden_files() {
        assert_eq!(sanitize_filename("photo.jpg"), "photo.jpg");
        assert_eq!(sanitize_filename(".bashrc"), ".bashrc");
        assert_eq!(sanitize_filename("a.tar.gz"), "a.tar.gz");
    }

    #[test]
    fn caps_pathological_length_keeping_ext() {
        let long = format!("{}.txt", "x".repeat(500));
        let out = sanitize_filename(&long);
        assert!(out.len() <= 200);
        assert!(out.ends_with(".txt"));
    }

    #[test]
    fn split_ext_handles_hidden_and_multi_dot() {
        assert_eq!(split_ext("a.txt"), ("a", ".txt"));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
        assert_eq!(split_ext("a.tar.gz"), ("a.tar", ".gz"));
        assert_eq!(split_ext("noext"), ("noext", ""));
    }

    #[test]
    fn with_counter_inserts_before_extension() {
        assert_eq!(with_counter("photo.jpg", 1), "photo (1).jpg");
        assert_eq!(with_counter("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(with_counter("noext", 1), "noext (1)");
    }

    #[test]
    fn with_counter_reuses_existing_suffix() {
        // The key anti-stacking property.
        assert_eq!(with_counter("photo (1).jpg", 2), "photo (2).jpg");
        assert_eq!(with_counter("photo (7).jpg", 3), "photo (3).jpg");
    }

    #[test]
    fn strip_counter_only_matches_real_suffix() {
        assert_eq!(strip_counter("photo (1)"), "photo");
        assert_eq!(strip_counter("photo"), "photo");
        assert_eq!(strip_counter("photo(1)"), "photo(1)"); // no space → not a suffix
        assert_eq!(strip_counter("photo ()"), "photo ()"); // no digit
        assert_eq!(strip_counter("(1)"), "(1)"); // no stem/space before
    }

    #[test]
    fn safe_save_path_dedups_with_counter() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();

        let p1 = safe_save_path(d, "doc.txt").unwrap();
        assert_eq!(p1.file_name().unwrap(), "doc.txt");
        std::fs::write(&p1, b"a").unwrap();

        let p2 = safe_save_path(d, "doc.txt").unwrap();
        assert_eq!(p2.file_name().unwrap(), "doc (1).txt");
        std::fs::write(&p2, b"b").unwrap();

        let p3 = safe_save_path(d, "doc.txt").unwrap();
        assert_eq!(p3.file_name().unwrap(), "doc (2).txt");
    }

    #[test]
    fn safe_save_path_stays_within_dir_for_traversal_name() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let p = safe_save_path(d, "../../../etc/passwd").unwrap();
        assert!(p.starts_with(d));
        assert_eq!(p.file_name().unwrap(), "passwd");
    }

    #[test]
    fn safe_save_path_sanitizes_absolute_name() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let p = safe_save_path(d, "/tmp/evil.sh").unwrap();
        assert!(p.starts_with(d));
        assert_eq!(p.file_name().unwrap(), "evil.sh");
    }
}
