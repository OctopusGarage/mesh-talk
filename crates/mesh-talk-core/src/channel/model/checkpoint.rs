//! Device-local channel metadata and private receiver preparation. This is not
//! a wire distribution or a portable backup; receiver history lives in SQL rows.
use super::*;

#[derive(Serialize, Deserialize)]
struct CurrentState {
    version: u8,
    id: ConversationId,
    name: String,
    name_version: (u64, EventId),
    members: Vec<PublicIdentity>,
    epoch: u64,
    owner: String,
    account_owner: Option<String>,
    account_members: HashMap<String, String>,
    dissolved: bool,
    genesis_owner: String,
    pending_invite: bool,
    accepted_event_id: Option<EventId>,
    accepted_members: HashSet<String>,
    my_user_id: Option<String>,
}

pub(crate) enum CheckpointMessageError {
    Rejected,
    Gap { after_position: u32 },
    Malformed(String),
}

pub(crate) struct PreparedChannelMessage {
    pub(crate) next_position: u32,
    pub(crate) epoch: u64,
    pub(crate) author: String,
    pub(crate) checkpoint: Zeroizing<Vec<u8>>,
    pub(crate) plaintext: Zeroizing<Vec<u8>>,
}

impl ChannelState {
    /// Current membership only. Epoch keys and processed identities are stored
    /// separately so this record cannot grow with retained message history.
    pub(crate) fn encode_current(&self) -> Result<Vec<u8>, ChannelError> {
        let current = CurrentState {
            version: 1,
            id: self.id,
            name: self.name.clone(),
            name_version: self.name_version,
            members: self.members.clone(),
            epoch: self.epoch,
            owner: self.owner.clone(),
            account_owner: self.account_owner.clone(),
            account_members: self.account_members.clone(),
            dissolved: self.dissolved,
            genesis_owner: self.genesis_owner.clone(),
            pending_invite: self.pending_invite,
            accepted_event_id: self.accepted_event_id,
            accepted_members: self.accepted_members.clone(),
            my_user_id: self.my_user_id.clone(),
        };
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&current)
            .map_err(|e| ChannelError::Malformed(e.to_string()))
    }

    pub(crate) fn decode_current(bytes: &[u8]) -> Result<Self, ChannelError> {
        let current: CurrentState = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| ChannelError::Malformed(e.to_string()))?;
        let mut members = HashSet::with_capacity(current.members.len());
        if current.version != 1
            || current
                .members
                .iter()
                .any(|member| !members.insert(member.user_id()))
        {
            return Err(ChannelError::Malformed(
                "invalid current channel checkpoint".into(),
            ));
        }
        Ok(Self {
            id: current.id,
            name: current.name,
            name_version: current.name_version,
            members: current.members,
            epoch: current.epoch,
            owner: current.owner,
            account_owner: current.account_owner,
            account_members: current.account_members,
            dissolved: current.dissolved,
            genesis_owner: current.genesis_owner,
            pending_invite: current.pending_invite,
            accepted_event_id: current.accepted_event_id,
            accepted_members: current.accepted_members,
            my_user_id: current.my_user_id,
            my_sender: HashMap::new(),
            sender_chains: HashMap::new(),
        })
    }

    pub(crate) fn checkpoint_distribution(
        bytes: &[u8],
    ) -> Result<SenderKeyDistribution, ChannelError> {
        SenderChain::decode_checkpoint(bytes)
            .map(|chain| chain.current_distribution())
            .map_err(|_| ChannelError::Malformed("invalid local receiver checkpoint".into()))
    }

    pub(crate) fn sender_message_epoch(expected_sender: &str, wire: &[u8]) -> Option<u64> {
        let (header, _) = message_parts(wire)?;
        (header.sender == expected_sender).then_some(header.epoch)
    }

    /// The caller loads one exact SQL receiver row. Preparation authenticates
    /// the claimed author before opening, and never mutates that durable row.
    pub(crate) fn prepare_checkpoint_message(
        expected_sender: &str,
        checkpoint: &[u8],
        wire: &[u8],
    ) -> Result<PreparedChannelMessage, CheckpointMessageError> {
        let (header, header_len) = message_parts(wire).ok_or(CheckpointMessageError::Rejected)?;
        if header.sender != expected_sender {
            return Err(CheckpointMessageError::Rejected);
        }
        let chain = SenderChain::decode_checkpoint(checkpoint).map_err(|_| {
            CheckpointMessageError::Malformed("invalid local receiver checkpoint".into())
        })?;
        let prepared = chain
            .prepare_open(header.n, &wire[2 + header_len..], &wire[2..2 + header_len])
            .map_err(|error| match error {
                crate::channel::sender_key::SenderKeyError::TooManySkipped => {
                    CheckpointMessageError::Gap {
                        after_position: header
                            .n
                            .saturating_sub(crate::channel::sender_key::MAX_SKIP),
                    }
                }
                _ => CheckpointMessageError::Rejected,
            })?;
        let (checkpoint, plaintext, next_position) = prepared.into_checkpoint().map_err(|_| {
            CheckpointMessageError::Malformed("invalid local receiver checkpoint".into())
        })?;
        Ok(PreparedChannelMessage {
            next_position,
            epoch: header.epoch,
            author: header.sender,
            checkpoint,
            plaintext,
        })
    }

    /// Authenticated receive-only snapshots may bridge an otherwise unopenable
    /// gap; the exact consumed holes and eviction order remain local.
    pub(crate) fn refreshed_receiver_checkpoint(
        checkpoint: &[u8],
        distribution: &SenderKeyDistribution,
    ) -> Result<Zeroizing<Vec<u8>>, ChannelError> {
        let mut chain = SenderChain::decode_checkpoint(checkpoint)
            .map_err(|_| ChannelError::Malformed("invalid local receiver checkpoint".into()))?;
        chain.refresh_distribution(distribution);
        chain
            .encode_checkpoint()
            .map_err(|_| ChannelError::Malformed("invalid local receiver checkpoint".into()))
    }

    /// The indexed runtime keeps exactly its active sending chain in RAM. All
    /// receive epochs, including the active one, belong to the SQL owner.
    pub(crate) fn use_indexed_receivers(&mut self) {
        self.sender_chains.clear();
        self.my_sender.retain(|epoch, _| *epoch == self.epoch);
    }

    pub(crate) fn initial_receiver_checkpoint(
        distribution: &SenderKeyDistribution,
    ) -> Result<Zeroizing<Vec<u8>>, ChannelError> {
        SenderChain::from_distribution(distribution)
            .encode_checkpoint()
            .map_err(|_| ChannelError::Malformed("invalid local receiver checkpoint".into()))
    }
}

fn message_parts(wire: &[u8]) -> Option<(MsgHeader, usize)> {
    let length = u16::from_be_bytes(wire.get(..2)?.try_into().ok()?) as usize;
    let header = MsgHeader::decode(wire.get(2..2 + length)?)?;
    Some((header, length))
}
