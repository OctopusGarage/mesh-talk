use mesh_replication::CompleteInventory;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Both sets must describe the same application-authorized scope.
    let local = CompleteInventory::from_ids(&[[1; 32], [2; 32]], 128)?;
    let peer = CompleteInventory::from_ids(&[[2; 32], [3; 32]], 128)?;
    let missing_at_peer: Vec<_> = local.missing_from(&peer).collect();
    assert_eq!(missing_at_peer, vec![[1; 32]]);
    assert_ne!(local.fingerprint(), peer.fingerprint());
    println!(
        "Peer is missing {} authorized event(s)",
        missing_at_peer.len()
    );
    Ok(())
}
