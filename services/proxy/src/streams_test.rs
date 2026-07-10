use std::sync::Arc;

use crate::streams::ActiveStreamTracker;

#[test]
fn active_stream_tracker_counts_active_streams() {
    let tracker = ActiveStreamTracker::new(2);

    let first_stream = tracker
        .try_start_stream()
        .expect("first stream should start");
    let second_stream = tracker
        .try_start_stream()
        .expect("second stream should start");

    assert_eq!(tracker.current(), 2);

    second_stream.finish();
    assert_eq!(tracker.current(), 1);

    drop(first_stream);
    assert_eq!(tracker.current(), 0);
}

#[test]
fn active_stream_tracker_rejects_streams_at_limit() {
    let tracker = ActiveStreamTracker::new(1);
    let _stream = tracker
        .try_start_stream()
        .expect("first stream should start");

    assert!(tracker.try_start_stream().is_none());
    assert_eq!(tracker.current(), 1);
}

#[test]
fn owned_active_stream_guard_counts_active_streams() {
    let tracker = Arc::new(ActiveStreamTracker::new(1));
    let stream = tracker
        .try_start_owned()
        .expect("owned stream should start");

    assert_eq!(tracker.current(), 1);
    assert!(tracker.try_start_owned().is_none());

    stream.finish();
    assert_eq!(tracker.current(), 0);
}
