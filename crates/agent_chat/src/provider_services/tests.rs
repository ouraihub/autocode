//! Behavioural tests for the GPUI credential broker.
//!
//! Round-trip semantics of `CredentialStore` live with `InMemoryVault` in
//! `model_provider::vault::tests`. This file only covers the pump lifecycle
//! against GPUI: per `docs/03-详细设计.md` §4.2 the GPUI test platform is a
//! no-op and is therefore not a valid read-back oracle.

use super::*;
use model_provider::{CredentialError, CredentialStore, Secret};
#[gpui::test]
async fn broker_reports_unavailable_when_task_dropped(cx: &mut gpui::TestAppContext) {
    let (broker, task) = cx.update(spawn_credential_broker);
    // Drop the task: the pump stops and the channel receiver dies.
    drop(task);
    // Give the executor a chance to drop the receiver before the next send.
    cx.run_until_parked();

    let err = broker
        .put("ref/x", Secret::new("v"))
        .await
        .expect_err("broker without a pump must fail");
    assert!(matches!(err, CredentialError::Unavailable), "got {err:?}");

    let err = broker
        .get("ref/x")
        .await
        .expect_err("broker without a pump must fail");
    assert!(matches!(err, CredentialError::Unavailable), "got {err:?}");

    let err = broker
        .delete("ref/x")
        .await
        .expect_err("broker without a pump must fail");
    assert!(matches!(err, CredentialError::Unavailable), "got {err:?}");
}

#[gpui::test]
async fn broker_forwards_to_gpui_platform_without_panicking(
    cx: &mut gpui::TestAppContext,
) {
    // The GPUI test platform returns Ok(None) / Ok(()) for every credential
    // call. We cannot assert round-trip here; the goal is to prove the pump
    // actually drives requests and does not deadlock while the task lives.
    let (broker, _task) = cx.update(spawn_credential_broker);

    // read -> None, not an error. Required by the trait contract.
    let got = broker.get("ref/y").await.unwrap();
    assert!(got.is_none(), "GPUI test platform returns None, not an error");

    // write + delete: platform accepts, broker does not translate Ok to error.
    broker
        .put("ref/y", Secret::new("v"))
        .await
        .expect("write passes through");
    broker
        .delete("ref/y")
        .await
        .expect("delete passes through");
}
