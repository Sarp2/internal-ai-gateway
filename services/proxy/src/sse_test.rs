use crate::sse::{event_data, take_next_event};

#[test]
fn takes_lf_delimited_events_incrementally() {
    let mut buffer = b"data: first\n\ndata: second".to_vec();

    assert_eq!(take_next_event(&mut buffer), Some(b"data: first".to_vec()));
    assert_eq!(buffer, b"data: second");
    assert_eq!(take_next_event(&mut buffer), None);

    buffer.extend_from_slice(b"\n\n");
    assert_eq!(take_next_event(&mut buffer), Some(b"data: second".to_vec()));
    assert!(buffer.is_empty());
}

#[test]
fn supports_crlf_and_mixed_line_endings() {
    for separator in [b"\r\n\r\n".as_slice(), b"\r\n\n", b"\n\r\n", b"\r\r"] {
        let mut buffer = [b"data: event".as_slice(), separator].concat();

        assert_eq!(take_next_event(&mut buffer), Some(b"data: event".to_vec()));
        assert!(buffer.is_empty());
    }
}

#[test]
fn joins_multiline_data_and_ignores_other_sse_fields() {
    let event = b"event: message\nid: 42\ndata: {\"first\":\ndata: true}";

    assert_eq!(event_data(event), Some(b"{\"first\":\ntrue}".to_vec()));
}

#[test]
fn extracts_multiline_data_with_cr_line_endings() {
    assert_eq!(
        event_data(b"data: first\rdata: second"),
        Some(b"first\nsecond".to_vec())
    );
}
