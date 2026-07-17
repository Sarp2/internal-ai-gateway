use std::io::{self, Read};

use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, timeout_at};

const BODY_CHANNEL_CAPACITY: usize = 4;
pub(crate) const BODY_CHUNK_BYTES: usize = 16 * 1024;

type InputChunk = io::Result<Bytes>;

pub(crate) fn timed_request_body_reader(
    body: Body,
    upload_timeout: Duration,
    timeout_message: &'static str,
) -> ChannelReader {
    let (sender, receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    tokio::spawn(pump_request_body(
        body,
        sender,
        upload_timeout,
        timeout_message,
    ));
    ChannelReader::new(receiver)
}

async fn pump_request_body(
    body: Body,
    sender: mpsc::Sender<InputChunk>,
    upload_timeout: Duration,
    timeout_message: &'static str,
) {
    let mut stream = body.into_data_stream();
    let deadline = Instant::now() + upload_timeout;

    loop {
        let result = match timeout_at(deadline, stream.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(_) => {
                let _ = sender
                    .send(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        timeout_message,
                    )))
                    .await;
                return;
            }
        };

        match result {
            Ok(chunk) => {
                for section in chunk.chunks(BODY_CHUNK_BYTES) {
                    if sender
                        .send(Ok(Bytes::copy_from_slice(section)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender
                    .send(Err(io::Error::new(io::ErrorKind::InvalidData, error)))
                    .await;
                return;
            }
        }
    }
}

pub(crate) struct ChannelReader {
    current: Bytes,
    offset: usize,
    receiver: mpsc::Receiver<InputChunk>,
}

impl ChannelReader {
    fn new(receiver: mpsc::Receiver<InputChunk>) -> Self {
        Self {
            current: Bytes::new(),
            offset: 0,
            receiver,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        while self.offset == self.current.len() {
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Some(Err(error)) => return Err(error),
                None => return Ok(0),
            }
        }

        let bytes_to_copy = output.len().min(self.current.len() - self.offset);
        output[..bytes_to_copy]
            .copy_from_slice(&self.current[self.offset..self.offset + bytes_to_copy]);
        self.offset += bytes_to_copy;
        Ok(bytes_to_copy)
    }
}
