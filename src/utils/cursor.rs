use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// Encodes a keyset-pagination cursor as opaque base64(JSON). JSON avoids any
/// need to pick a delimiter that's guaranteed absent from the underlying
/// fields (e.g. a merchant name can legitimately contain any character).
pub fn encode<T: Serialize>(value: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("cursor is serializable"))
}

pub fn decode<T: DeserializeOwned>(cursor: &str) -> Result<T, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    serde_json::from_slice(&bytes).map_err(|_| AppError::BadRequest("invalid cursor".into()))
}

#[cfg(test)]
mod tests {
    use quickcheck::{Arbitrary, Gen};

    use super::*;

    /// A stand-in for a real cursor payload (e.g. `TransactionCursor`) —
    /// exercises the same generic encode/decode path with a type that's
    /// cheap to generate arbitrarily, since `quickcheck` has no `Arbitrary`
    /// impl for `Uuid`/`NaiveDate` out of the box.
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct SamplePayload {
        id: u64,
        label: String,
    }

    impl Arbitrary for SamplePayload {
        fn arbitrary(g: &mut Gen) -> Self {
            Self {
                id: u64::arbitrary(g),
                label: String::arbitrary(g),
            }
        }
    }

    // By-value is required: cast to `fn(SamplePayload) -> bool` below,
    // matching `quickcheck::Testable`'s signature for a property fn.
    #[allow(clippy::needless_pass_by_value)]
    fn prop_encode_then_decode_round_trips(payload: SamplePayload) -> bool {
        decode::<SamplePayload>(&encode(&payload)).is_ok_and(|decoded| decoded == payload)
    }

    #[test]
    fn encode_then_decode_round_trips() {
        crate::utils::proptest_support::run(
            "encode_then_decode_round_trips",
            prop_encode_then_decode_round_trips as fn(SamplePayload) -> bool,
        );
    }

    #[test]
    fn decode_rejects_a_string_thats_not_valid_base64() {
        assert!(decode::<SamplePayload>("not valid base64!").is_err());
    }
}
