use crate::storage::errors::StorageError;
use bincode;
use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct SerializedData<T> {
    pub version: u32,
    pub data: T,
}

pub fn serialize_data<T>(data: &T) -> Result<Vec<u8>, StorageError>
where
    T: Serialize,
{
    let serialized_data = SerializedData {
        version: CURRENT_VERSION,
        data,
    };

    bincode::serialize(&serialized_data).map_err(|e| StorageError::Serialization(e.to_string()))
}

pub fn deserialize_data<T>(bytes: &[u8]) -> Result<T, StorageError>
where
    T: for<'de> Deserialize<'de>,
{
    let serialized_data: SerializedData<T> =
        bincode::deserialize(bytes).map_err(|e| StorageError::Deserialization(e.to_string()))?;

    // In the future, we can handle version upgrades here
    if serialized_data.version > CURRENT_VERSION {
        return Err(StorageError::Deserialization(
            "Data version is newer than supported version".to_string(),
        ));
    }

    Ok(serialized_data.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_garbage_and_future_version() {
        let bytes = serialize_data(&vec![1u8, 2, 3]).unwrap();
        let back: Vec<u8> = deserialize_data(&bytes).unwrap();
        assert_eq!(back, vec![1, 2, 3]);

        // Garbage bytes fail to deserialize.
        assert!(deserialize_data::<Vec<u8>>(b"not a valid frame").is_err());

        // A version newer than CURRENT_VERSION is rejected.
        let future = bincode::serialize(&SerializedData {
            version: CURRENT_VERSION + 1,
            data: vec![9u8],
        })
        .unwrap();
        assert!(deserialize_data::<Vec<u8>>(&future).is_err());
    }
}
