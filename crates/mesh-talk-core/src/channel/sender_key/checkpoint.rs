//! Private device-local receiver codec; never a wire distribution or backup.
//! Skipped entries are unique positions below next_n in exact eviction order.
//! Order need not be sorted: decoding must never rebuild it from map iteration.

use super::{MAX_SKIPPED_TOTAL, SenderChain, SenderKeyError};
use std::collections::{HashMap, HashSet, VecDeque};
use zeroize::Zeroizing;

const MAGIC: &[u8; 5] = b"MTRX\x01";
const HEADER: usize = 5 + 32 + 4 + 4;
const ENTRY: usize = 4 + 32;
const MAX_BYTES: usize = HEADER + MAX_SKIPPED_TOTAL * ENTRY;

fn invalid() -> SenderKeyError {
    SenderKeyError::Malformed("invalid local receiver checkpoint".into())
}

impl SenderChain {
    pub(super) fn encode_checkpoint(&self) -> Result<Zeroizing<Vec<u8>>, SenderKeyError> {
        if self.skipped.len() > MAX_SKIPPED_TOTAL || self.order.len() != self.skipped.len() {
            return Err(invalid());
        }
        let mut seen = HashSet::with_capacity(self.order.len());
        // Validate before copying any secrets into the output.
        for position in &self.order {
            if *position >= self.n
                || !self.skipped.contains_key(position)
                || !seen.insert(*position)
            {
                return Err(invalid());
            }
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(HEADER + self.order.len() * ENTRY));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.chain_key);
        bytes.extend_from_slice(&self.n.to_be_bytes());
        bytes.extend_from_slice(&(self.order.len() as u32).to_be_bytes());
        for position in &self.order {
            bytes.extend_from_slice(&position.to_be_bytes());
            bytes.extend_from_slice(&self.skipped[position]);
        }
        Ok(bytes)
    }

    pub(super) fn decode_checkpoint(bytes: &[u8]) -> Result<Self, SenderKeyError> {
        // No allocation depends on the declared count until size and count have
        // both been checked against the fixed local bound and exact input length.
        if !(HEADER..=MAX_BYTES).contains(&bytes.len()) || &bytes[..5] != MAGIC {
            return Err(invalid());
        }
        let n = u32::from_be_bytes(bytes[37..41].try_into().map_err(|_| invalid())?);
        let count = u32::from_be_bytes(bytes[41..45].try_into().map_err(|_| invalid())?) as usize;
        if count > MAX_SKIPPED_TOTAL || bytes.len() != HEADER + count * ENTRY {
            return Err(invalid());
        }
        let key = Zeroizing::new(<[u8; 32]>::try_from(&bytes[5..37]).map_err(|_| invalid())?);
        let mut chain = Self {
            chain_key: *key,
            n,
            skipped: HashMap::with_capacity(count),
            order: VecDeque::with_capacity(count),
        };
        for entry in bytes[HEADER..].as_chunks::<ENTRY>().0 {
            let position = u32::from_be_bytes(entry[..4].try_into().map_err(|_| invalid())?);
            if position >= n || chain.skipped.contains_key(&position) {
                return Err(invalid());
            }
            let key = Zeroizing::new(<[u8; 32]>::try_from(&entry[4..]).map_err(|_| invalid())?);
            chain.skipped.insert(position, *key);
            chain.order.push_back(position);
        }
        Ok(chain)
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn receiver() -> SenderChain {
        SenderChain::from_distribution(&SenderKeyDistribution {
            chain_key: [23; 32],
            n: 0,
        })
    }

    #[test]
    fn checkpoint_preserves_consumed_holes_and_exact_eviction_order() {
        let mut original = receiver();
        original.message_key(3).unwrap();
        original.message_key(1).unwrap();
        // The codec must preserve queue order, not sort a map's keys.
        original.order.swap(0, 1);
        let bytes = original.encode_checkpoint().unwrap();
        let mut restored = SenderChain::decode_checkpoint(&bytes).unwrap();
        assert_eq!(restored.order, original.order);
        for position in [1, 3] {
            assert!(restored.message_key(position).is_err());
        }
        for target in [1004, 2005, 3006] {
            assert_eq!(
                restored.message_key(target).unwrap(),
                original.message_key(target).unwrap()
            );
        }
        assert_eq!(restored.order, original.order);
        assert_eq!(restored.skipped, original.skipped);
        assert_eq!(restored.skipped.len(), MAX_SKIPPED_TOTAL);
        let positions: Vec<_> = restored.order.iter().copied().collect();
        for position in positions {
            assert_eq!(
                restored.message_key(position).unwrap(),
                original.message_key(position).unwrap()
            );
            assert!(restored.message_key(position).is_err());
        }
    }

    #[test]
    fn checkpoint_reopens_delayed_keys_and_authenticated_refresh() {
        let mut original = receiver();
        original.message_key(3).unwrap();
        original.message_key(1).unwrap();
        let mut restored =
            SenderChain::decode_checkpoint(&original.encode_checkpoint().unwrap()).unwrap();
        restored.refresh_distribution(&SenderKeyDistribution {
            chain_key: [41; 32],
            n: 2000,
        });
        for position in [0, 2] {
            assert_eq!(
                restored.message_key(position).unwrap(),
                original.message_key(position).unwrap()
            );
            assert!(restored.message_key(position).is_err());
        }
        assert!(restored.message_key(1).is_err());
        assert!(restored.message_key(3).is_err());
        let mut expected = SenderChain::from_distribution(&SenderKeyDistribution {
            chain_key: [41; 32],
            n: 2000,
        });
        assert_eq!(
            restored.message_key(2000).unwrap(),
            expected.message_key(2000).unwrap()
        );
    }

    #[test]
    fn checkpoint_rejects_invalid_lengths_versions_positions_and_duplicates() {
        let mut original = receiver();
        original.message_key(3).unwrap();
        let valid = original.encode_checkpoint().unwrap();
        for end in 0..valid.len() {
            assert!(SenderChain::decode_checkpoint(&valid[..end]).is_err());
        }
        let mut invalid = valid.to_vec();
        invalid.push(0);
        assert!(SenderChain::decode_checkpoint(&invalid).is_err());
        invalid = valid.to_vec();
        invalid[4] = 255;
        assert!(SenderChain::decode_checkpoint(&invalid).is_err());
        invalid = valid.to_vec();
        invalid[41..45].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(SenderChain::decode_checkpoint(&invalid).is_err());
        invalid = valid.to_vec();
        invalid[45..49].copy_from_slice(&4u32.to_be_bytes());
        assert!(SenderChain::decode_checkpoint(&invalid).is_err());
        invalid = valid.to_vec();
        let duplicate = invalid[45..49].to_vec();
        invalid[81..85].copy_from_slice(&duplicate);
        assert!(SenderChain::decode_checkpoint(&invalid).is_err());
        assert!(SenderChain::decode_checkpoint(&vec![0; 80_000]).is_err());
    }

    #[test]
    fn checkpoint_rejects_inconsistent_live_map_order() {
        let mut original = receiver();
        original.message_key(3).unwrap();
        original.order.push_back(0);
        assert!(original.encode_checkpoint().is_err());
        original.order.pop_back();
        original.order[0] = 1;
        assert!(original.encode_checkpoint().is_err());
    }
}
