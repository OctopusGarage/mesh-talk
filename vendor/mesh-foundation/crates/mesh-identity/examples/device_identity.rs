use mesh_identity::device::DeviceIdentity;

fn main() {
    let device = DeviceIdentity::generate();
    let public = device.public();
    let message = b"hello mesh";
    let signature = device.sign(message);
    assert!(DeviceIdentity::verify(
        &public.ed25519_pub,
        message,
        &signature
    ));
    assert!(!DeviceIdentity::verify(
        &public.ed25519_pub,
        b"tampered",
        &signature
    ));
    println!("Device {} signed and verified a message", public.user_id());
}
