//! Pairing by short authentication string (spec §10).
//!
//! Both peers already know each other's certificate fingerprint from mutual
//! TLS. The initiator commits to its nonce before seeing the responder's, so a
//! man in the middle cannot grind nonces until the two 6-digit codes match;
//! the users comparing the codes on both screens closes the loop.

use crate::util::sha256;
use sha2::{Digest, Sha256};

pub const PAIR_TIMEOUT_SECS: u64 = 120;
pub const MAX_PENDING: usize = 8;

pub fn commitment(nonce: &[u8; 32]) -> [u8; 32] {
    sha256(nonce)
}

/// The 6-digit code both screens show. `nonce_a` is the initiator's.
pub fn sas_code(fp_a: &str, fp_b: &str, nonce_a: &[u8; 32], nonce_b: &[u8; 32]) -> u32 {
    let (a, b) = (fp_a.to_ascii_uppercase(), fp_b.to_ascii_uppercase());
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(b"FSPAIR1");
    h.update(lo.as_bytes());
    h.update(hi.as_bytes());
    h.update(nonce_a);
    h.update(nonce_b);
    let d = h.finalize();
    u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000
}

/// `"012 345"`.
pub fn format_sas(code: u32) -> String {
    let s = format!("{code:06}");
    format!("{} {}", &s[..3], &s[3..])
}

#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    TooMany,
    Unknown,
    Expired,
    BadCommitment,
    WrongPeer,
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PairError {}

#[derive(Clone, Debug)]
pub struct Pending {
    pub pair_id: String,
    pub peer_fp: String,
    pub alias: String,
    commitment: [u8; 32],
    nonce_b: [u8; 32],
    created: u64,
    pub sas: Option<u32>,
}

/// The responder's pairings in progress.
#[derive(Default)]
pub struct PendingPairings {
    items: Vec<Pending>,
}

impl PendingPairings {
    fn purge(&mut self, now: u64) {
        self.items.retain(|p| now.saturating_sub(p.created) < PAIR_TIMEOUT_SECS);
    }

    /// Step 1 (commit). A new request from the same peer replaces its older
    /// one, so there is at most one per peer. Returns `(pair_id, nonce_b)`.
    pub fn begin(
        &mut self,
        peer_fp: &str,
        alias: &str,
        commitment: [u8; 32],
        nonce_b: [u8; 32],
        now: u64,
    ) -> Result<String, PairError> {
        self.purge(now);
        self.items.retain(|p| !p.peer_fp.eq_ignore_ascii_case(peer_fp));
        if self.items.len() >= MAX_PENDING {
            return Err(PairError::TooMany);
        }
        let pair_id = uuid::Uuid::new_v4().to_string();
        self.items.push(Pending {
            pair_id: pair_id.clone(),
            peer_fp: peer_fp.to_ascii_uppercase(),
            alias: alias.chars().take(crate::settings::MAX_ALIAS_CHARS).collect(),
            commitment,
            nonce_b,
            created: now,
            sas: None,
        });
        Ok(pair_id)
    }

    /// Step 2 (reveal): checks the commitment and computes the code.
    pub fn reveal(
        &mut self,
        pair_id: &str,
        peer_fp: &str,
        nonce_a: &[u8; 32],
        own_fp: &str,
        now: u64,
    ) -> Result<Pending, PairError> {
        let index =
            self.items.iter().position(|p| p.pair_id == pair_id).ok_or(PairError::Unknown)?;
        let item = &self.items[index];
        if now.saturating_sub(item.created) >= PAIR_TIMEOUT_SECS {
            self.items.remove(index);
            return Err(PairError::Expired);
        }
        if !item.peer_fp.eq_ignore_ascii_case(peer_fp) {
            return Err(PairError::WrongPeer);
        }
        if item.sas.is_some() {
            // Single use: a repeated reveal would prompt the user again.
            return Err(PairError::Unknown);
        }
        if commitment(nonce_a) != item.commitment {
            self.items.remove(index);
            return Err(PairError::BadCommitment);
        }
        let sas = sas_code(peer_fp, own_fp, nonce_a, &item.nonce_b);
        self.items[index].sas = Some(sas);
        Ok(self.items[index].clone())
    }

    /// Removes a revealed, unexpired pairing for the confirm step.
    pub fn take(&mut self, pair_id: &str, peer_fp: &str, now: u64) -> Option<Pending> {
        self.purge(now);
        let index = self.items.iter().position(|p| {
            p.pair_id == pair_id && p.peer_fp.eq_ignore_ascii_case(peer_fp) && p.sas.is_some()
        })?;
        Some(self.items.remove(index))
    }

    /// Seconds left before `pending` expires.
    pub fn remaining(pending: &Pending, now: u64) -> u64 {
        PAIR_TIMEOUT_SECS.saturating_sub(now.saturating_sub(pending.created))
    }

    pub fn nonce_b(&self, pair_id: &str) -> Option<[u8; 32]> {
        self.items.iter().find(|p| p.pair_id == pair_id).map(|p| p.nonce_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FA: &str = "AAAA";
    const FB: &str = "BBBB";

    #[test]
    fn sas_symmetric() {
        let (na, nb) = ([1u8; 32], [2u8; 32]);
        assert_eq!(sas_code(FA, FB, &na, &nb), sas_code(FB, FA, &na, &nb));
        assert_eq!(sas_code(FA, FB, &na, &nb), sas_code("aaaa", "bbbb", &na, &nb));
        assert_ne!(sas_code(FA, FB, &na, &nb), sas_code(FA, FB, &nb, &na));
    }

    #[test]
    fn sas_known_vector() {
        // Pinned: changing the derivation breaks pairing between versions.
        assert_eq!(sas_code(FA, FB, &[1u8; 32], &[2u8; 32]), KNOWN);
    }
    const KNOWN: u32 = 215_774;

    #[test]
    fn format_pads_zeros() {
        assert_eq!(format_sas(12_345), "012 345");
        assert_eq!(format_sas(999_999), "999 999");
    }

    fn begin(p: &mut PendingPairings, peer: &str, nonce_a: [u8; 32], now: u64) -> String {
        p.begin(peer, "PC", commitment(&nonce_a), [9u8; 32], now).unwrap()
    }

    #[test]
    fn happy_path_matches_initiator_code() {
        let mut p = PendingPairings::default();
        let id = begin(&mut p, FA, [1u8; 32], 0);
        let pending = p.reveal(&id, FA, &[1u8; 32], FB, 5).unwrap();
        assert_eq!(pending.sas, Some(sas_code(FA, FB, &[1u8; 32], &[9u8; 32])));
        assert_eq!(
            p.reveal(&id, FA, &[1u8; 32], FB, 6).unwrap_err(),
            PairError::Unknown,
            "single use"
        );
        let taken = p.take(&id, FA, 10).unwrap();
        assert_eq!(PendingPairings::remaining(&taken, 10), PAIR_TIMEOUT_SECS - 10);
        assert!(p.take(&id, FA, 10).is_none());
    }

    #[test]
    fn confirm_after_expiry_rejected() {
        let mut p = PendingPairings::default();
        let id = begin(&mut p, FA, [1u8; 32], 0);
        p.reveal(&id, FA, &[1u8; 32], FB, 5).unwrap();
        assert!(p.take(&id, FA, PAIR_TIMEOUT_SECS).is_none());
    }

    #[test]
    fn bad_commitment_rejected() {
        let mut p = PendingPairings::default();
        let id = begin(&mut p, FA, [1u8; 32], 0);
        assert_eq!(p.reveal(&id, FA, &[7u8; 32], FB, 1).unwrap_err(), PairError::BadCommitment);
        assert_eq!(p.reveal(&id, FA, &[1u8; 32], FB, 1).unwrap_err(), PairError::Unknown);
    }

    #[test]
    fn expires_after_120s() {
        let mut p = PendingPairings::default();
        let id = begin(&mut p, FA, [1u8; 32], 0);
        assert_eq!(p.reveal(&id, FA, &[1u8; 32], FB, 120).unwrap_err(), PairError::Expired);
    }

    #[test]
    fn one_pending_per_peer() {
        let mut p = PendingPairings::default();
        let first = begin(&mut p, FA, [1u8; 32], 0);
        let second = begin(&mut p, FA, [1u8; 32], 1);
        assert_eq!(p.reveal(&first, FA, &[1u8; 32], FB, 2).unwrap_err(), PairError::Unknown);
        assert!(p.reveal(&second, FA, &[1u8; 32], FB, 2).is_ok());
    }

    #[test]
    fn max_eight_pending() {
        let mut p = PendingPairings::default();
        for i in 0..MAX_PENDING {
            begin(&mut p, &format!("P{i}"), [1u8; 32], 0);
        }
        let err = p.begin("P9", "PC", [0; 32], [0; 32], 0).unwrap_err();
        assert_eq!(err, PairError::TooMany);
        assert!(p.begin("P9", "PC", [0; 32], [0; 32], 200).is_ok(), "expired ones are purged");
    }

    #[test]
    fn reveal_from_other_peer_rejected() {
        let mut p = PendingPairings::default();
        let id = begin(&mut p, FA, [1u8; 32], 0);
        assert_eq!(p.reveal(&id, "CCCC", &[1u8; 32], FB, 1).unwrap_err(), PairError::WrongPeer);
    }
}
