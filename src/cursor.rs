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
