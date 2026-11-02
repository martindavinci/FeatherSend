//! This device's identity as the GUI shows it, and network change handling.

mod common;

use common::*;

#[tokio::test]
async fn identity_announced_at_start() {
    let mut node = NodeSpec::new(0).start().await;
    let identity = node
        .wait_for(|r| match r {
            Rec::Identity(view) => Some(view.clone()),
            _ => None,
        })
        .await;
    assert_eq!(identity.alias, "node0");
    assert_eq!(identity.port, node.port);
    assert!(identity.addresses.iter().all(|a| !a.is_loopback()));
    assert!(!identity.primary || !identity.addresses.is_empty());
}
