//! Peer readiness must come from the CLI roster, not unrelated live output.
pub(super) fn is_peer_listing(line: &str, expected_user_id: &str) -> bool {
    let mut fields = line.split_whitespace();
    fields.next() == Some("peer") && fields.next() == Some(expected_user_id)
}
