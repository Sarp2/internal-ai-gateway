const EVENT_SEPARATORS: [&[u8]; 5] = [b"\r\n\r\n", b"\r\n\n", b"\n\r\n", b"\n\n", b"\r\r"];

pub(crate) fn take_next_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (event_end, separator_length) = EVENT_SEPARATORS
        .iter()
        .filter_map(|separator| {
            buffer
                .windows(separator.len())
                .position(|window| window == *separator)
                .map(|position| (position, separator.len()))
        })
        .min_by_key(|(position, _)| *position)?;

    let event = buffer.drain(..event_end).collect::<Vec<_>>();
    buffer.drain(..separator_length);
    Some(event)
}

pub(crate) fn event_data(event: &[u8]) -> Option<Vec<u8>> {
    let event_text = String::from_utf8_lossy(event);
    let data_lines = event_text
        .split(['\r', '\n'])
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>();

    (!data_lines.is_empty()).then(|| data_lines.join("\n").into_bytes())
}
