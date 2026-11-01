//! JSON bodies of the extension protocol (camelCase on the wire).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoDto {
    pub alias: String,
    pub fingerprint: String,
    pub version: String,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCommitReq {
    pub alias: String,
    /// Hex SHA-256 of the initiator's nonce.
    pub commitment: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairCommitResp {
    pub pair_id: String,
    /// Hex.
    pub nonce_b: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRevealReq {
    pub pair_id: String,
    /// Hex.
    pub nonce_a: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairConfirmReq {
    pub pair_id: String,
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairConfirmResp {
    pub accepted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Purpose {
    Files,
    Clipboard,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtFile {
    pub id: String,
    /// Relative path with `/` separators, including the root folder name.
    pub path: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the whole file.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReq {
    pub purpose: Purpose,
    pub files: Vec<ExtFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareResp {
    pub transfer_id: String,
    /// Bytes the receiver already has, per accepted file id. Files missing
    /// from the map were not accepted.
    pub offsets: HashMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishResp {
    pub completed: Vec<String>,
    pub failed: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dto_json_shape() {
        let json = serde_json::to_value(PrepareReq {
            purpose: Purpose::Clipboard,
            files: vec![ExtFile { id: "1".into(), path: "a/b.txt".into(), size: 3, sha256: None }],
        })
        .unwrap();
        assert_eq!(json["purpose"], "clipboard");
        assert_eq!(json["files"][0]["path"], "a/b.txt");
        let resp =
            serde_json::to_value(PairCommitResp { pair_id: "p".into(), nonce_b: "n".into() })
                .unwrap();
        assert_eq!(resp["pairId"], "p");
        assert_eq!(resp["nonceB"], "n");
    }
}
