use mesh_talk_protocol::{account::Account, device::DeviceIdentity, legacy_event::EventKind};
#[test]
fn historic_account_id_certificate_domain_and_event_tag_remain_unchanged() {
    let account = Account::from_secret_bytes([1; 32]);
    assert_eq!(account.account_id(), "6e6c0cda1153dc2a8523c88fd35ed4ea");
    let device = DeviceIdentity::generate();
    let certificate = account.certify(&device.public().ed25519_pub);
    let mut signed = b"mesh-talk-device-cert-v1".to_vec();
    signed.extend_from_slice(&account.public().ed25519_pub);
    signed.extend_from_slice(&device.public().ed25519_pub);
    assert!(DeviceIdentity::verify(
        &account.public().ed25519_pub,
        &signed,
        &certificate.signature
    ));
    assert_eq!(
        bincode::serialize(&EventKind::ChannelMetadata).unwrap(),
        19u32.to_le_bytes()
    );
}
#[test]
fn historic_discovery_and_mailbox_framing_stay_application_owned() {
    use mesh_talk_protocol::{
        discovery::announce::{Announce, encode},
        legacy_event::ConversationId,
        mailbox::rpc::{Frame, encode_frame},
    };
    let identity = DeviceIdentity::generate();
    assert_eq!(
        &encode(&Announce::new(&identity, "legacy", 1234))[..5],
        b"MTAN\x02"
    );
    let bytes = encode_frame(&Frame::Supported {
        conversation: ConversationId::new([7; 32]),
    })
    .unwrap();
    assert_eq!(&bytes[..5], b"MTOM1");
}

#[test]
fn historic_device_fingerprint_stays_unchanged() {
    let identity = DeviceIdentity::from_secret_bytes([1; 32], [2; 32]);
    assert_eq!(identity.user_id(), "fa868a6961f6455e7f59ef951793ef2b");
}

#[test]
fn independent_historical_certificate_vector_preserves_exact_bytes() {
    // Frozen independently with Python cryptography's Ed25519 implementation.
    let account = Account::from_secret_bytes([1; 32]);
    let device = DeviceIdentity::from_secret_bytes([3; 32], [2; 32]);
    let account_public =
        hex::decode("8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c").unwrap();
    let device_public =
        hex::decode("ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1").unwrap();
    let signature = hex::decode("97ec7bb87966dc3700a57548884cfe265eea010308c4e4784615ae786344da1012e98871f695f7d35aae6441ed5b3ed3cf8b2d1c24116c4f4559fbc29d9b7808").unwrap();
    assert_eq!(account.public().ed25519_pub.as_slice(), account_public);
    assert_eq!(device.public().ed25519_pub.as_slice(), device_public);
    let certificate = account.certify(&device.public().ed25519_pub);
    assert_eq!(certificate.signature.as_slice(), signature);
    let expected = [account_public, device_public, signature].concat();
    assert_eq!(certificate.encode(), expected);
    assert_eq!(
        mesh_talk_protocol::account::DeviceCertificate::decode(&expected),
        Some(certificate.clone())
    );
    let mut neutral = b"mesh-foundation-device-cert-v1".to_vec();
    neutral.extend_from_slice(&certificate.account_ed25519_pub);
    neutral.extend_from_slice(&certificate.device_ed25519_pub);
    assert!(!DeviceIdentity::verify(
        &certificate.account_ed25519_pub,
        &neutral,
        &certificate.signature
    ));
    assert_eq!(account.secret_bytes(), [1; 32]);
    assert_eq!(device.secret_bytes(), ([3; 32], [2; 32]));
}

#[test]
fn delegated_verifier_rejects_noncanonical_and_small_order_signatures() {
    let identity = DeviceIdentity::from_secret_bytes([3; 32], [2; 32]);
    let message = b"strict legacy verification";
    let mut signature = identity.sign(message);
    assert!(DeviceIdentity::verify(
        &identity.public().ed25519_pub,
        message,
        &signature
    ));
    // Ed25519 scalars must be canonical: S = group order L is invalid.
    signature[32..].copy_from_slice(
        &hex::decode("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010").unwrap(),
    );
    assert!(!DeviceIdentity::verify(
        &identity.public().ed25519_pub,
        message,
        &signature
    ));
    // The compressed identity point has order one. Even an S=0,R=identity
    // construction must never establish an author's identity.
    let mut weak_public = [0; 32];
    weak_public[0] = 1;
    let mut weak_signature = [0; 64];
    weak_signature[0] = 1;
    assert!(!DeviceIdentity::verify(
        &weak_public,
        message,
        &weak_signature
    ));
}
