use mesh_talk_core::recovery::key::RecoveryKey;

#[test]
fn key_round_trips_in_grouped_display_form() {
    let generated = RecoveryKey::generate();
    let displayed = generated.display();
    assert_eq!(
        displayed.chars().filter(|c| c.is_ascii_hexdigit()).count(),
        64
    );
    let parsed = RecoveryKey::parse(&displayed).unwrap();
    assert_eq!(parsed.as_bytes(), generated.as_bytes());
}

#[test]
fn pasted_key_accepts_spaces_and_uppercase() {
    let key = RecoveryKey::parse(&"AB ".repeat(32)).unwrap();
    assert_eq!(key.as_bytes(), &[0xab; 32]);
}

#[test]
fn short_or_non_hex_key_is_rejected() {
    assert!(RecoveryKey::parse("1234").is_err());
    assert!(RecoveryKey::parse(&"zz".repeat(32)).is_err());
}

#[test]
fn debug_never_prints_the_key() {
    let key = RecoveryKey::parse(&"ab".repeat(32)).unwrap();
    let debug = format!("{key:?}");
    assert!(!debug.contains("abab"));
}
