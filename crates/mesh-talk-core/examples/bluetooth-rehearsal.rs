//! Explicit two-host radio rehearsal, isolated from production chat state.
use clap::{Parser, ValueEnum};
use mesh_talk_core::identity::{device::PublicIdentity, keystore};
use mesh_talk_core::transport::SecureChannel;
use std::{
    io,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Central,
    Peripheral,
}
#[derive(Parser)]
struct Args {
    #[arg(long)]
    keystore: PathBuf,
    #[arg(long)]
    password_file: PathBuf,
    /// Print public keys without starting Bluetooth.
    #[arg(long)]
    identity: bool,
    #[arg(long)]
    radio: Option<PathBuf>,
    #[arg(long, value_enum)]
    role: Option<Role>,
    /// Expected peer Ed25519 then X25519 public keys (128 hex characters).
    #[arg(long)]
    peer: Option<String>,
    #[arg(long, default_value = "Mesh-Talk BLE rehearsal")]
    message: String,
}
struct RadioIo {
    input: tokio::process::ChildStdin,
    output: tokio::process::ChildStdout,
}
impl AsyncRead for RadioIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.output).poll_read(cx, buf)
    }
}
impl AsyncWrite for RadioIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.input).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.input).poll_shutdown(cx)
    }
}
fn expected(value: &str) -> Result<PublicIdentity, Box<dyn std::error::Error>> {
    if value.len() != 128 {
        return Err("--peer requires 128 hex characters".into());
    }
    let bytes = hex::decode(value)?;
    Ok(PublicIdentity {
        ed25519_pub: bytes[..32].try_into()?,
        x25519_pub: bytes[32..].try_into()?,
    })
}
async fn interrupted() -> io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        let mut hup = signal(SignalKind::hangup())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = term.recv() => Ok(()),
            _ = hup.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let password = std::fs::read_to_string(&args.password_file)?;
    let identity =
        keystore::load_or_create(&args.keystore, password.trim_end_matches(['\r', '\n']))?;
    let public = identity.public();
    if args.identity {
        println!(
            "{}{}",
            hex::encode(public.ed25519_pub),
            hex::encode(public.x25519_pub)
        );
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        return Err("native BLE rehearsal currently requires macOS".into());
    }
    let peer = expected(
        args.peer
            .as_deref()
            .ok_or("--peer is required; exchange keys out of band first")?,
    )?;
    let role = args.role.ok_or("--role is required")?;
    if args.message.len() > 4096 {
        return Err("rehearsal text exceeds 4096 bytes".into());
    }
    let mut child = tokio::process::Command::new(args.radio.ok_or("--radio is required")?)
        .arg(match role {
            Role::Central => "central",
            Role::Peripheral => "peripheral",
        })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let stream = RadioIo {
        input: child.stdin.take().ok_or("missing radio stdin")?,
        output: child.stdout.take().ok_or("missing radio stdout")?,
    };
    let operation = async {
        let mut channel = match role {
            Role::Central => SecureChannel::connect(stream, &identity, Some(&peer)).await?,
            Role::Peripheral => {
                let ch = SecureChannel::accept(stream, &identity).await?;
                if ch.peer_identity() != &peer {
                    return Err("unexpected peer identity".into());
                }
                ch
            }
        };
        match role {
            Role::Central => {
                channel.send(args.message.as_bytes()).await?;
                let echo = channel.recv().await?;
                if echo != args.message.as_bytes() {
                    return Err("peer echo mismatch".into());
                }
                eprintln!(
                    "Authenticated BLE round trip verified (memory only; no durable chat receipt)."
                );
            }
            Role::Peripheral => {
                let bytes = channel.recv().await?;
                if bytes.len() > 4096 {
                    return Err("peer rehearsal text exceeds 4096 bytes".into());
                }
                eprintln!(
                    "Authenticated text received: {}",
                    String::from_utf8_lossy(&bytes)
                );
                channel.send(&bytes).await?;
                // Wait for initiator to close after receiving the echo.
                let _ = channel.recv().await;
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(120), operation) => result.map_err(|_| "BLE rehearsal timed out".into()).and_then(|v| v),
        _ = interrupted() => Err("BLE rehearsal cancelled".into()),
    };
    let _ = child.start_kill();
    let retired = child.wait().await;
    result?;
    retired?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_keys_reject_wrong_lengths_and_non_hex() {
        assert!(expected("").is_err());
        assert!(expected(&"00".repeat(63)).is_err());
        assert!(expected(&"zz".repeat(64)).is_err());
        let peer = expected(&"01".repeat(64)).unwrap();
        assert_eq!(peer.ed25519_pub, [1; 32]);
    }
}
