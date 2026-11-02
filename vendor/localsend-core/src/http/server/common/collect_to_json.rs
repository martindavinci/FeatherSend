use crate::http::server::common::error::AppError;
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

/// Modified by FeatherSend (see VENDORED.md): a request body is held in memory,
/// so its size is limited. Large enough for a prepare-upload of tens of
/// thousands of files.
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;

pub(crate) trait CollectToJson {
    async fn collect_to_json<T: DeserializeOwned>(self) -> Result<T, AppError>;
}

impl CollectToJson for Incoming {
    async fn collect_to_json<T: DeserializeOwned>(self) -> Result<T, AppError> {
        let bytes = Limited::new(self, MAX_JSON_BODY)
            .collect()
            .await
            .map_err(|err| {
                tracing::warn!("Refused a JSON body: {err:#}");
                AppError::BadRequest("Body too large or unreadable".to_string())
            })?
            .to_bytes();
        serde_json::from_slice::<T>(&bytes).map_err(|err| {
            tracing::warn!("Failed to parse JSON body: {err:#}");
            AppError::BadRequest("Invalid JSON body".to_string())
        })
    }
}
