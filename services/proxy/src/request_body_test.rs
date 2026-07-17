use std::io::{self, Read};
use std::time::Duration;

use axum::body::{Body, Bytes};
use futures_util::stream;
use tokio::sync::mpsc;

use crate::request_body::{BODY_CHUNK_BYTES, test_channel_reader, timed_request_body_reader};

#[test]
fn empty_reads_return_immediately() {
    let (sender, receiver) = mpsc::channel(1);
    let mut reader = test_channel_reader(receiver);

    assert_eq!(reader.read(&mut []).unwrap(), 0);
    drop(sender);
}

#[test]
fn reads_channel_bytes_before_clean_eof() {
    let (sender, receiver) = mpsc::channel(1);
    sender
        .blocking_send(Bytes::from_static(b"request"))
        .unwrap();
    drop(sender);
    let mut reader = test_channel_reader(receiver);
    let mut output = Vec::new();

    reader.read_to_end(&mut output).unwrap();

    assert_eq!(output, b"request");
}

#[tokio::test]
async fn upload_deadline_interrupts_a_blocked_channel_send() {
    let body = Body::from_stream(stream::iter([Ok::<_, io::Error>(Bytes::from(vec![
        b'x';
        BODY_CHUNK_BYTES
            * 5
    ]))]));
    let mut reader =
        timed_request_body_reader(body, Duration::from_millis(10), "request upload timed out");

    tokio::time::sleep(Duration::from_millis(30)).await;
    let error = tokio::task::spawn_blocking(move || {
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .expect_err("blocked channel send should preserve the upload timeout")
    })
    .await
    .unwrap();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}
