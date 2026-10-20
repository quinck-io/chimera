use std::time::Duration;

use super::JobCancel;

#[tokio::test]
async fn request_stops_steps_that_do_not_survive_cancel() {
    let cancel = JobCancel::new();
    let step_token = cancel.step_token(false);

    cancel.request(Duration::from_secs(60));

    assert!(cancel.is_requested());
    assert!(step_token.is_cancelled());
}

#[tokio::test]
async fn request_spares_steps_that_survive_cancel() {
    let cancel = JobCancel::new();
    let step_token = cancel.step_token(true);

    cancel.request(Duration::from_secs(60));

    assert!(!step_token.is_cancelled());
}

#[tokio::test]
async fn step_started_after_request_survives_if_allowed() {
    let cancel = JobCancel::new();
    cancel.request(Duration::from_secs(60));

    assert!(!cancel.step_token(true).is_cancelled());
    assert!(cancel.step_token(false).is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn grace_period_end_stops_surviving_steps() {
    let cancel = JobCancel::new();
    let step_token = cancel.step_token(true);

    cancel.request(Duration::from_secs(45));
    tokio::time::sleep(Duration::from_secs(44)).await;
    assert!(!step_token.is_cancelled());

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(step_token.is_cancelled());
}

#[tokio::test]
async fn force_stops_every_step() {
    let cancel = JobCancel::new();
    let surviving = cancel.step_token(true);
    let regular = cancel.step_token(false);

    cancel.force();

    assert!(cancel.is_requested());
    assert!(surviving.is_cancelled());
    assert!(regular.is_cancelled());
}
