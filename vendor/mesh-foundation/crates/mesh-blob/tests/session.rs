#![cfg(feature = "mesh-connect")]

use mesh_blob::{
    BlobLayout, DirectoryChunkStore, EncryptedChunkSource, FileKey, SparseBlobReceiver, chunk_hash,
    file_checksum, seal_chunk_indexed,
    session::{SessionChunkSource, serve_request},
};
use mesh_connect::PeerSession;
use mesh_identity::device::DeviceIdentity;
use std::{num::NonZeroUsize, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    task::LocalSet,
};

#[tokio::test]
async fn authenticated_tcp_sources_supply_complementary_chunks() {
    let object = file_checksum(b"abcdef");
    let file_key = FileKey::generate();
    let file_nonce = [3; 8];
    let client = DeviceIdentity::generate();
    let client_key = client.public().x25519_pub;
    let mut sources = Vec::new();
    let mut servers = Vec::new();
    LocalSet::new()
        .run_until(async {
            for (index, bytes) in [b"ab", b"cd", b"ef"].into_iter().enumerate() {
                let ciphertext =
                    seal_chunk_indexed(&file_key, &file_nonce, index as u32, bytes).unwrap();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let source = DeviceIdentity::generate();
                let public = source.public().x25519_pub;
                servers.push(tokio::task::spawn_local(async move {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut session =
                        PeerSession::accept(socket, &source.secret_bytes().1, |peer| {
                            *peer == client_key
                        })
                        .await
                        .unwrap();
                    for _ in 0..2 {
                        serve_request(
                            &mut session,
                            |peer, requested, chunk, length| {
                                assert_eq!(*peer, client_key);
                                assert_eq!(requested, object);
                                assert_eq!(chunk as usize, index);
                                assert_eq!(length, 18);
                                Some(ciphertext.clone())
                            },
                            |peer, requested, start, length| {
                                assert_eq!(*peer, client_key);
                                assert_eq!(requested, object);
                                assert_eq!((start, length), (0, 3));
                                Some(
                                    (start..start + length)
                                        .map(|chunk| chunk as usize == index)
                                        .collect(),
                                )
                            },
                        )
                        .await
                        .unwrap();
                    }
                }));
                let socket = TcpStream::connect(address).await.unwrap();
                let session = PeerSession::connect(socket, &client.secret_bytes().1, &public)
                    .await
                    .unwrap();
                sources.push(EncryptedChunkSource::new(
                    SessionChunkSource::discover(object, 3, 3, session, Duration::from_secs(2))
                        .await
                        .unwrap(),
                    &file_key,
                    file_nonce,
                ));
            }
            let directory = tempfile::tempdir().unwrap();
            let mut store = DirectoryChunkStore::open(directory.path()).unwrap();
            let layout = BlobLayout::new(6, 2, 6).unwrap();
            let hashes = [chunk_hash(b"ab"), chunk_hash(b"cd"), chunk_hash(b"ef")];
            SparseBlobReceiver::new(layout, object, &hashes, 3)
                .unwrap()
                .download(&mut store, &sources, NonZeroUsize::new(3).unwrap())
                .await
                .unwrap();

            for server in servers {
                server.await.unwrap();
            }
        })
        .await;
}
