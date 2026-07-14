use crate::anthropic_request::inspect_slice;

#[test]
fn extracts_reservation_controls_without_changing_the_request() {
    let (streaming, max_tokens, image_inputs) = inspect_slice(
        br#"{"model":"claude-sonnet-4-5","max_tokens":4096,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
    )
    .expect("request metadata should parse");

    assert!(streaming);
    assert_eq!(max_tokens, 4096);
    assert_eq!(image_inputs, 0);
}

#[test]
fn counts_anthropic_image_content_blocks() {
    let (_, _, image_inputs) = inspect_slice(
        br#"{"model":"claude-sonnet-4-5","max_tokens":1024,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"https://example.com/one.png"}},{"type":"text","text":"compare"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"abc"}}]}]}"#,
    )
    .expect("image request should parse");

    assert_eq!(image_inputs, 2);
}

#[test]
fn rejects_missing_or_invalid_max_tokens() {
    assert!(inspect_slice(br#"{"model":"claude-sonnet-4-5","messages":[]}"#).is_err());
    assert!(
        inspect_slice(br#"{"model":"claude-sonnet-4-5","max_tokens":0,"messages":[]}"#).is_err()
    );
}

#[test]
fn rejects_duplicate_max_tokens() {
    assert!(
        inspect_slice(
            br#"{"model":"claude-sonnet-4-5","max_tokens":100,"max_tokens":200,"messages":[]}"#
        )
        .is_err()
    );
}
