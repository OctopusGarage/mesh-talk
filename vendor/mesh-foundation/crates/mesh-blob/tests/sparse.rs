use mesh_blob::{
    BlobLayout, ChunkAcceptance, ChunkStore, FileError, SparseBlobReceiver, SparseReceiveError,
    chunk_hash, file_checksum,
};
use std::collections::BTreeMap;

#[derive(Default)]
struct Store {
    chunks: BTreeMap<u32, Vec<u8>>,
    fail_write: bool,
}

impl ChunkStore for Store {
    type Error = ();

    fn read(&mut self, index: u32, _: u32) -> Result<Option<Vec<u8>>, ()> {
        Ok(self.chunks.get(&index).cloned())
    }

    fn write(&mut self, index: u32, bytes: &[u8]) -> Result<(), ()> {
        if self.fail_write {
            return Err(());
        }
        self.chunks.insert(index, bytes.to_vec());
        Ok(())
    }
}

#[test]
fn sparse_progress_survives_receiver_recreation_and_rejects_bad_chunks() {
    let layout = BlobLayout::new(5, 2, 5).unwrap();
    let hashes = [chunk_hash(b"ab"), chunk_hash(b"cd"), chunk_hash(b"e")];
    let open = || SparseBlobReceiver::new(layout, file_checksum(b"abcde"), &hashes, 3).unwrap();
    let mut store = Store::default();
    let mut receive = open();
    receive.accept(&mut store, 2, b"e").unwrap();
    receive.accept(&mut store, 0, b"ab").unwrap();
    assert!(receive.verify(&mut store).is_err());

    let mut receive = open();
    receive.restore(&mut store).unwrap();
    assert_eq!(receive.missing().collect::<Vec<_>>(), [1]);
    assert!(matches!(
        receive.accept(&mut store, 1, b"xx"),
        Err(SparseReceiveError::Invalid(FileError::ChunkHashMismatch(1)))
    ));
    store.fail_write = true;
    assert!(matches!(
        receive.accept(&mut store, 1, b"cd"),
        Err(SparseReceiveError::Store(()))
    ));
    assert_eq!(receive.missing().collect::<Vec<_>>(), [1]);
    store.fail_write = false;
    receive.accept(&mut store, 1, b"cd").unwrap();
    assert_eq!(
        receive.accept(&mut store, 1, b"cd").unwrap(),
        ChunkAcceptance::AlreadyStored
    );
    receive.verify(&mut store).unwrap();

    store.chunks.insert(0, b"xx".to_vec());
    assert!(receive.verify(&mut store).is_err());
    receive.restore(&mut store).unwrap();
    assert_eq!(receive.missing().collect::<Vec<_>>(), [0]);
}

#[tokio::test]
async fn complementary_sources_retry_and_resume_from_disk() {
    use mesh_blob::{ChunkSource, DirectoryChunkStore, DownloadError};
    use std::{cell::RefCell, num::NonZeroUsize};

    struct Source<'a> {
        chunks: &'a [(u32, &'a [u8])],
        requests: &'a RefCell<Vec<u32>>,
    }

    impl ChunkSource for Source<'_> {
        type Error = ();

        fn has_chunk(&self, index: u32) -> bool {
            self.chunks.iter().any(|(key, _)| *key == index)
        }

        async fn fetch(&self, index: u32, _: u32) -> Result<Vec<u8>, ()> {
            self.requests.borrow_mut().push(index);
            tokio::task::yield_now().await;
            self.chunks
                .iter()
                .find(|(key, _)| *key == index)
                .map(|(_, data)| data.to_vec())
                .ok_or(())
        }
    }

    let path = tempfile::tempdir().unwrap();
    let layout = BlobLayout::new(5, 2, 5).unwrap();
    let hashes = [chunk_hash(b"ab"), chunk_hash(b"cd"), chunk_hash(b"e")];
    let open = || SparseBlobReceiver::new(layout, file_checksum(b"abcde"), &hashes, 3).unwrap();
    use mesh_blob::{ChunkRejection, DownloadEvent};
    let events = RefCell::new(Vec::new());
    let requests = RefCell::new(Vec::new());
    let limit = NonZeroUsize::new(3).unwrap();
    {
        let mut store = DirectoryChunkStore::open(path.path()).unwrap();
        let mut receiver = open();
        let peers = [
            Source {
                chunks: &[(0, b"xx")],
                requests: &requests,
            },
            Source {
                chunks: &[(0, b"ab")],
                requests: &requests,
            },
            Source {
                chunks: &[(2, b"e")],
                requests: &requests,
            },
        ];
        assert!(matches!(
            receiver
                .download_observed(&mut store, &peers, limit, |event| events
                    .borrow_mut()
                    .push(event))
                .await,
            Err(DownloadError::Unavailable(1))
        ));
    }
    assert!(events.borrow().contains(&DownloadEvent::SourceRejected {
        index: 0,
        reason: ChunkRejection::HashMismatch,
    }));
    assert!(!events.borrow().contains(&DownloadEvent::Verified));
    events.borrow_mut().clear();
    requests.borrow_mut().clear();
    let mut store = DirectoryChunkStore::open(path.path()).unwrap();
    let mut receiver = open();
    receiver.restore(&mut store).unwrap();
    assert_eq!(receiver.missing().collect::<Vec<_>>(), [1]);
    let peer = Source {
        chunks: &[(1, b"cd")],
        requests: &requests,
    };
    assert_eq!(receiver.stored_chunks(), 2);
    receiver
        .download_observed(&mut store, &[peer], limit, |event| {
            events.borrow_mut().push(event)
        })
        .await
        .unwrap();
    assert_eq!(
        *events.borrow(),
        [
            DownloadEvent::ChunkStored { index: 1, bytes: 2 },
            DownloadEvent::Verified,
        ]
    );
    assert_eq!(*requests.borrow(), [1]);
}

#[tokio::test]
async fn a_slow_source_does_not_block_refilling_free_request_slots() {
    use mesh_blob::ChunkSource;
    use std::{num::NonZeroUsize, time::Duration};
    use tokio::sync::Notify;

    struct Source<'a> {
        slow: bool,
        fast_started_next: &'a Notify,
    }

    impl ChunkSource for Source<'_> {
        type Error = ();

        fn has_chunk(&self, index: u32) -> bool {
            self.slow == (index == 0)
        }

        async fn fetch(&self, index: u32, _: u32) -> Result<Vec<u8>, ()> {
            match index {
                0 => self.fast_started_next.notified().await,
                2 => self.fast_started_next.notify_one(),
                _ => {}
            }
            Ok(vec![index as u8])
        }
    }

    let fast_started_next = Notify::new();
    let sources = [true, false].map(|slow| Source {
        slow,
        fast_started_next: &fast_started_next,
    });
    let bytes = [0, 1, 2];
    let layout = BlobLayout::new(3, 1, 3).unwrap();
    let hashes = bytes.map(|byte| chunk_hash(&[byte]));
    let mut receiver = SparseBlobReceiver::new(layout, file_checksum(&bytes), &hashes, 3).unwrap();
    let mut store = Store::default();

    tokio::time::timeout(
        Duration::from_secs(2),
        receiver.download(&mut store, &sources, NonZeroUsize::new(2).unwrap()),
    )
    .await
    .expect("the free slot must start chunk 2 while chunk 0 is still pending")
    .unwrap();
}
