//! A human-comparable rendering of a peer's fingerprint (its `user_id`, the hex of a
//! SHA-256 prefix of its Ed25519 key). Two users compare these out-of-band (read it
//! aloud, scan it) to confirm there is no man-in-the-middle. This is purely a
//! presentation of the EXISTING fingerprint — it adds no new crypto and changes no key
//! material; it only makes the same bytes easier to eyeball.

/// A short, fixed pool of common, phonetically-distinct words. Chosen for at-a-glance
/// comparison; the security still rests on the full fingerprint, this is a quick
/// "do these look the same?" aid. 32 words → 5 bits each.
const WORDS: [&str; 32] = [
    "alpha", "bravo", "cedar", "delta", "ember", "falcon", "grove", "harbor", "indigo", "jasmine",
    "koala", "lemon", "maple", "nectar", "ocean", "pepper", "quartz", "raven", "sierra", "tiger",
    "umber", "violet", "willow", "xenon", "yarrow", "zephyr", "amber", "basil", "coral", "dune",
    "echo", "frost",
];

/// Group a fingerprint into space-separated 5-char blocks for readability:
/// `abcd1234…` → `abcd1 234…`. Deterministic and reversible (just strip spaces).
pub fn grouped(fingerprint: &str) -> String {
    let mut out = String::with_capacity(fingerprint.len() + fingerprint.len() / 5);
    for (i, c) in fingerprint.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A deterministic short word sequence derived from the fingerprint, for a quick
/// "these match" check that's easier than reading hex. Decodes the leading hex of the
/// fingerprint into 5-bit indices over [`WORDS`]. Non-hex / short input yields fewer
/// words (empty if none decodes). `count` words are produced (or fewer if the
/// fingerprint runs out of bits).
pub fn words(fingerprint: &str, count: usize) -> Vec<&'static str> {
    // Collect the hex nibbles as a bit stream.
    let mut bits: Vec<u8> = Vec::new();
    for c in fingerprint.chars() {
        let Some(nibble) = c.to_digit(16) else {
            continue;
        };
        for shift in (0..4).rev() {
            bits.push(((nibble >> shift) & 1) as u8);
        }
    }
    let mut out = Vec::with_capacity(count);
    for chunk in bits.chunks(5) {
        if out.len() == count {
            break;
        }
        if chunk.len() < 5 {
            break; // drop a trailing partial group
        }
        let idx = chunk.iter().fold(0usize, |acc, &b| (acc << 1) | b as usize);
        out.push(WORDS[idx]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_inserts_spaces_every_five() {
        assert_eq!(grouped("abcdefghij"), "abcde fghij");
        assert_eq!(grouped("abc"), "abc");
        assert_eq!(grouped("abcdef"), "abcde f");
    }

    #[test]
    fn grouped_is_deterministic() {
        let fp = "0123456789abcdef0123456789abcdef";
        assert_eq!(grouped(fp), grouped(fp));
    }

    #[test]
    fn words_are_deterministic_and_bounded() {
        let fp = "0123456789abcdef0123456789abcdef";
        let a = words(fp, 4);
        let b = words(fp, 4);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn words_differ_for_different_fingerprints() {
        let a = words("0000000000000000", 4);
        let b = words("ffffffffffffffff", 4);
        assert_ne!(a, b);
        // all-zero → index 0 word repeated
        assert_eq!(a, vec!["alpha", "alpha", "alpha", "alpha"]);
    }

    #[test]
    fn words_handles_short_or_nonhex_input() {
        assert!(words("", 4).is_empty());
        assert!(words("xyz", 4).is_empty()); // no hex digits
                                             // one hex digit = 4 bits < 5, so no full word
        assert!(words("a", 4).is_empty());
    }
}
