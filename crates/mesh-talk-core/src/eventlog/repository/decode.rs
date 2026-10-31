//! Allocation preflight for the component's version-1 stored Event encoding.
//! The authoritative Event deserializer still follows this check. Read only
//! fixed-size existing types and skip borrowed variable fields; never forward an
//! unverified sequence size_hint to Serde's allocating Vec visitor.
use super::*;
use serde::de::DeserializeOwned;

fn scalar<T: DeserializeOwned>(input: &mut &[u8]) -> Result<T, LogError> {
    let limit =
        u64::try_from(input.len()).map_err(|_| corrupt("event encoding exceeds decoder limit"))?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(limit)
        .allow_trailing_bytes()
        .deserialize_from(input)
        .map_err(|error| LogError::Serialization(error.to_string()))
}

fn skip_vector(input: &mut &[u8], expected: usize, width: usize) -> Result<(), LogError> {
    let count: u64 = scalar(input)?;
    let expected = u64::try_from(expected)
        .map_err(|_| corrupt("indexed vector length exceeds encoding range"))?;
    if count != expected {
        return Err(corrupt(
            "encoded vector count disagrees with indexed metadata",
        ));
    }
    let bytes = count
        .checked_mul(u64::try_from(width).map_err(|_| corrupt("invalid vector width"))?)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| corrupt("encoded vector length overflows"))?;
    *input = input
        .get(bytes..)
        .ok_or_else(|| corrupt("encoded vector exceeds remaining event bytes"))?;
    Ok(())
}

pub(super) fn preflight(header: &EventHeader, bytes: &[u8]) -> Result<(), LogError> {
    let mut input = bytes;
    let id: EventId = scalar(&mut input)?;
    let conversation: ConversationId = scalar(&mut input)?;
    let author: Author = scalar(&mut input)?;
    let seq: u64 = scalar(&mut input)?;
    skip_vector(&mut input, header.parent_count, header.id.as_bytes().len())?;
    let lamport: u64 = scalar(&mut input)?;
    let wall_clock: u64 = scalar(&mut input)?;
    let kind: EventKind = scalar(&mut input)?;
    skip_vector(&mut input, header.ciphertext_bytes, 1)?;
    skip_vector(&mut input, header.signature_bytes, 1)?;
    if !input.is_empty()
        || id != header.id
        || conversation != header.conversation
        || author != header.author
        || seq != header.seq
        || lamport != header.lamport
        || wall_clock != header.wall_clock
        || kind != header.kind
    {
        return Err(corrupt("event encoding and indexed metadata disagree"));
    }
    Ok(())
}
