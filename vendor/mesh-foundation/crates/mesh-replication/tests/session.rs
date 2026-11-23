use mesh_replication::session::*;
use std::collections::VecDeque;

struct Session(VecDeque<Vec<u8>>);

impl ReconciliationSession for Session {
    type Peer = ();
    type Error = &'static str;

    fn peer_identity(&self) -> &Self::Peer {
        &()
    }

    async fn send(&mut self, _frame: &[u8]) -> Result<(), Self::Error> {
        unreachable!("the receiver never writes")
    }

    async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.0.pop_front().ok_or("closed")
    }
}

fn decode(frame: &[u8]) -> Result<HaveFrame<u8>, &'static str> {
    let (more, ids) = frame.split_first().ok_or("missing header")?;
    Ok(HaveFrame {
        ids: ids.to_vec(),
        more: *more != 0,
    })
}

fn limits() -> HaveLimits {
    HaveLimits {
        ids_per_frame: 2,
        frames: 3,
        total_ids: 3,
    }
}

#[tokio::test]
async fn preserves_duplicates_and_accepts_exact_terminal_boundary() {
    let mut session = Session([vec![1, 8, 8], vec![0, 9]].into());
    assert_eq!(
        receive_have(&mut session, limits(), decode).await.unwrap(),
        [8, 8, 9]
    );
}

#[tokio::test]
async fn rejects_per_frame_total_and_continuation_limits() {
    for chunks in [
        vec![vec![0, 1, 2, 3]],
        vec![vec![1, 1, 2], vec![0, 3, 4]],
        vec![vec![1], vec![1], vec![1]],
    ] {
        let mut session = Session(chunks.into());
        assert_eq!(
            receive_have(&mut session, limits(), decode).await,
            Err(HaveError::Limit)
        );
    }
}

#[tokio::test]
async fn preserves_decode_failure_and_premature_close() {
    for (chunks, expected) in [
        (vec![Vec::new()], "missing header"),
        (vec![vec![1, 1]], "closed"),
    ] {
        let mut session = Session(chunks.into());
        assert_eq!(
            receive_have(&mut session, limits(), decode).await,
            Err(HaveError::Exchange(expected))
        );
    }
}

struct Source(VecDeque<HavePage<u8, usize>>, bool);

impl HaveSource for Source {
    type Id = u8;
    type Cursor = usize;
    type Error = &'static str;

    fn page(
        &mut self,
        _: Option<&usize>,
        ids: usize,
        _: usize,
    ) -> Result<HavePage<u8, usize>, Self::Error> {
        let page = self.0.pop_front().ok_or("no page")?;
        assert!(page.ids.len() <= ids);
        Ok(page)
    }

    fn validate(&self) -> Result<(), Self::Error> {
        if self.1 {
            Err("authority retired")
        } else {
            Ok(())
        }
    }
}

struct Writer(Vec<Vec<u8>>);

impl ReconciliationSession for Writer {
    type Peer = ();
    type Error = &'static str;

    fn peer_identity(&self) -> &Self::Peer {
        &()
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.0.push(frame.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
        unreachable!()
    }
}

fn encode(frame: HaveFrame<u8>) -> Result<Vec<u8>, &'static str> {
    let mut bytes = vec![u8::from(frame.more)];
    bytes.extend(frame.ids);
    Ok(bytes)
}

#[tokio::test]
async fn sends_nonterminal_frames_only_after_lookahead_and_checks_exact_count() {
    let mut source = Source(
        [
            HavePage {
                ids: vec![1, 2],
                next: Some(1),
            },
            HavePage {
                ids: vec![],
                next: Some(2),
            },
            HavePage {
                ids: vec![3],
                next: None,
            },
        ]
        .into(),
        false,
    );
    let mut writer = Writer(Vec::new());
    send_have(&mut writer, &mut source, 3, limits(), encode, || {
        std::future::ready(())
    })
    .await
    .unwrap();
    assert_eq!(writer.0, [vec![1, 1, 2], vec![0, 3]]);

    let mut source = Source(
        [HavePage {
            ids: vec![],
            next: None,
        }]
        .into(),
        false,
    );
    let mut writer = Writer(Vec::new());
    assert_eq!(
        send_have(&mut writer, &mut source, 1, limits(), encode, || {
            std::future::ready(())
        })
        .await,
        Err(HaveError::CountMismatch)
    );
    assert!(writer.0.is_empty());
}

#[tokio::test]
async fn rechecks_authority_before_sending_even_an_empty_terminal() {
    let mut source = Source(
        [HavePage {
            ids: vec![],
            next: None,
        }]
        .into(),
        true,
    );
    let mut writer = Writer(Vec::new());
    assert_eq!(
        send_have(&mut writer, &mut source, 0, limits(), encode, || {
            std::future::ready(())
        })
        .await,
        Err(HaveError::Exchange("authority retired"))
    );
    assert!(writer.0.is_empty());
}

struct ExportFixture(VecDeque<u8>);

impl ExportSource for ExportFixture {
    type Event = u8;
    type Cursor = usize;
    type Error = &'static str;

    fn page(
        &mut self,
        cursor: &mut Option<usize>,
        space: PageSpace,
    ) -> Result<EventPage<u8, usize>, ExportError<Self::Error>> {
        let event = *self.0.front().ok_or(ExportError::Source("no row"))?;
        let charge = usize::from(event);
        if charge > space.encoded_bytes {
            return Err(ExportError::PageTooSmall("record does not fit"));
        }
        self.0.pop_front();
        Ok(EventPage {
            events: vec![event],
            next: (!self.0.is_empty()).then(|| cursor.unwrap_or(0) + 1),
            encoded_bytes: charge,
            allocated_bytes: charge,
            frame_finished: false,
        })
    }
}

fn export_limits() -> ExportLimits {
    ExportLimits {
        frames: 2,
        examined: 2,
        events_per_frame: 2,
        encoded_per_frame: 5,
        allocated_per_frame: 16,
    }
}

#[tokio::test]
async fn export_retries_the_unselected_row_in_the_next_frame() {
    let mut source = ExportFixture([3, 4, 2].into());
    let frames = select_export_frames(&mut source, export_limits(), || std::future::ready(()))
        .await
        .unwrap();
    assert_eq!(frames, [vec![3], vec![4]]);
    assert_eq!(source.0.front(), Some(&2));
}

#[tokio::test]
async fn export_returns_the_source_error_when_one_row_does_not_fit() {
    let mut source = ExportFixture([8].into());
    let result =
        select_export_frames(&mut source, export_limits(), || std::future::ready(())).await;
    assert_eq!(
        result,
        Err(ExportError::PageTooSmall("record does not fit"))
    );
    assert_eq!(source.0.front(), Some(&8));
}
