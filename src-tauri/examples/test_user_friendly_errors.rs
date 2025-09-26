use mesh_talk::error::{MeshTalkError, NetworkErrorKind};
use mesh_talk::user_friendly_errors::format_user_friendly_error;

fn main() {
    let error = MeshTalkError::network(
        NetworkErrorKind::ConnectionFailed,
        "Could not connect to 192.168.1.100:7000",
    );

    let friendly_message = format_user_friendly_error(&error);
    println!("{}", friendly_message);
}
