use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, timeout_at};

const BODY_CHANNEL_CAPACITY: usize = 4;
pub(crate) const BODY_CHUNK_BYTES: usize = 16 * 1024;

type TerminalError = Arc<Mutex<Option<io::Error>>>;

pub(crate) fn timed_request_body_reader(
    body: Body,
    upload_timeout: Duration,
    timeout_message: &'static str,
) -> ChannelReader {
    let (sender, receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    let terminal_error = Arc::new(Mutex::new(None));
    tokio::spawn(pump_request_body(
        body,
        sender,
        upload_timeout,
        timeout_message,
        Arc::clone(&terminal_error),
    ));
    ChannelReader::new(receiver, terminal_error)
}

async fn pump_request_body(
    body: Body,
    sender: mpsc::Sender<Bytes>,
    upload_timeout: Duration,
    timeout_message: &'static str,
    terminal_error: TerminalError,
) {
    let mut stream = body.into_data_stream();
    let deadline = Instant::now() + upload_timeout;

    loop {
        if Instant::now() >= deadline {
            set_terminal_error(&terminal_error, io::ErrorKind::TimedOut, timeout_message);
            return;
        }

        let result = match timeout_at(deadline, stream.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(_) => {
                set_terminal_error(&terminal_error, io::ErrorKind::TimedOut, timeout_message);
                return;
            }
        };

        if Instant::now() >= deadline {
            set_terminal_error(&terminal_error, io::ErrorKind::TimedOut, timeout_message);
            return;
        }

        match result {
            Ok(chunk) => {
                for section in chunk.chunks(BODY_CHUNK_BYTES) {
                    match timeout_at(deadline, sender.send(Bytes::copy_from_slice(section))).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => return,
                        Err(_) => {
                            set_terminal_error(
                                &terminal_error,
                                io::ErrorKind::TimedOut,
                                timeout_message,
                            );
                            return;
                        }
                    }
                }
            }
            Err(error) => {
                set_terminal_error(
                    &terminal_error,
                    io::ErrorKind::InvalidData,
                    error.to_string(),
                );
                return;
            }
        }
    }
}

fn set_terminal_error(
    terminal_error: &TerminalError,
    kind: io::ErrorKind,
    message: impl Into<String>,
) {
    *terminal_error
        .lock()
        .expect("request body terminal error lock should not be poisoned") =
        Some(io::Error::new(kind, message.into()));
}

pub(crate) struct ChannelReader {
    current: Bytes,
    offset: usize,
    receiver: mpsc::Receiver<Bytes>,
    terminal_error: TerminalError,
}

impl ChannelReader {
    fn new(receiver: mpsc::Receiver<Bytes>, terminal_error: TerminalError) -> Self {
        Self {
            current: Bytes::new(),
            offset: 0,
            receiver,
            terminal_error,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }

        while self.offset == self.current.len() {
            match self.receiver.blocking_recv() {
                Some(chunk) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                None => {
                    return match self
                        .terminal_error
                        .lock()
                        .expect("request body terminal error lock should not be poisoned")
                        .take()
                    {
                        Some(error) => Err(error),
                        None => Ok(0),
                    };
                }
            }
        }

        let bytes_to_copy = output.len().min(self.current.len() - self.offset);
        output[..bytes_to_copy]
            .copy_from_slice(&self.current[self.offset..self.offset + bytes_to_copy]);
        self.offset += bytes_to_copy;
        Ok(bytes_to_copy)
    }
}

#[cfg(test)]
pub(crate) fn test_channel_reader(receiver: mpsc::Receiver<Bytes>) -> ChannelReader {
    ChannelReader::new(receiver, Arc::new(Mutex::new(None)))
}
