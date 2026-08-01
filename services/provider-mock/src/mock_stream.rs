use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Bytes;
use futures_core::Stream;
use tokio::time::{Sleep, sleep};

pub(crate) struct DelayedByteStream {
    delay: Duration,
    events: std::vec::IntoIter<Bytes>,
    sleep: Option<Pin<Box<Sleep>>>,
}

impl DelayedByteStream {
    pub(crate) fn new(events: Vec<Bytes>, delay: Duration) -> Self {
        Self {
            delay,
            events: events.into_iter(),
            sleep: None,
        }
    }
}

impl Stream for DelayedByteStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.events.len() == 0 {
            return Poll::Ready(None);
        }
        if self.delay.is_zero() {
            return Poll::Ready(self.events.next().map(Ok));
        }

        if self.sleep.is_none() {
            self.sleep = Some(Box::pin(sleep(self.delay)));
        }
        let sleep = self
            .sleep
            .as_mut()
            .expect("stream delay should exist before polling");
        if sleep.as_mut().poll(context).is_pending() {
            return Poll::Pending;
        }
        self.sleep = None;

        Poll::Ready(self.events.next().map(Ok))
    }
}

pub(crate) fn split_text(text: &str, chunk_count: usize) -> Vec<String> {
    let characters = text.chars().collect::<Vec<_>>();
    let base_chunk_size = characters.len() / chunk_count;
    let larger_chunk_count = characters.len() % chunk_count;
    let mut offset = 0;

    (0..chunk_count)
        .map(|index| {
            let chunk_size = base_chunk_size + usize::from(index < larger_chunk_count);
            let chunk = characters[offset..offset + chunk_size].iter().collect();
            offset += chunk_size;
            chunk
        })
        .collect()
}
