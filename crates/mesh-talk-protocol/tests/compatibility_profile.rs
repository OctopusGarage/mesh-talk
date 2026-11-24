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
