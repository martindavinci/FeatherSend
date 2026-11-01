//! Pairing between two FeatherSend nodes over the extension protocol.

mod common;

use common::*;
use feathersend_core::backend::{Command, PairOutcome};
use feathersend_core::settings::DataDirs;
use feathersend_core::trust::TrustStore;

fn trusts(node: &Node, fp: &str) -> bool {
    TrustStore::load(&DataDirs::at(node.dir.path().join("data")).trusted_file).is_trusted(fp)
}

async fn code_and_result(node: &mut Node) -> (String, PairOutcome) {
    let code = node
        .wait_for(|r| match r {
            Rec::PairCode(c) => Some(c.clone()),
            _ => None,
        })
        .await;
    let outcome = node
        .wait_for(|r| match r {
            Rec::PairResult(o) => Some(*o),
            _ => None,
        })
        .await;
    (code, outcome)
}

#[tokio::test]
async fn pair_happy_path_both_trusted() {
    let mut a = NodeSpec::new(0).start().await;
    let mut b = NodeSpec::new(1).start().await;
    a.add(&b, true).await;
    a.handle.send(Command::Pair { device: b.fp.clone() });
    let (code_a, outcome_a) = code_and_result(&mut a).await;
    let (code_b, outcome_b) = code_and_result(&mut b).await;
    assert_eq!(code_a, code_b, "both screens show the same code");
    assert_eq!(code_a.len(), 7, "format \"123 456\"");
    assert_eq!((outcome_a, outcome_b), (PairOutcome::Paired, PairOutcome::Paired));
    assert!(trusts(&a, &b.fp));
    assert!(trusts(&b, &a.fp));
}

#[tokio::test]
async fn pair_rejected_by_responder_nobody_trusted() {
    let mut a = NodeSpec::new(0).start().await;
    let mut b = NodeSpec::new(1).answer(Answer::No).start().await;
    a.add(&b, true).await;
    a.handle.send(Command::Pair { device: b.fp.clone() });
    let (_, outcome_a) = code_and_result(&mut a).await;
    let (_, outcome_b) = code_and_result(&mut b).await;
    assert_eq!((outcome_a, outcome_b), (PairOutcome::Failed, PairOutcome::Failed));
    assert!(!trusts(&a, &b.fp));
    assert!(!trusts(&b, &a.fp));
}
