//! The inbox: messages and tool calls waiting for this agent.
//!
//! External AIs (via MCP or the HTTP API) poll the inbox; SDK users can
//! subscribe to it as a stream.

use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

/// Maximum items kept; the oldest are dropped first.
const INBOX_CAPACITY: usize = 1000;

/// Something that arrived for this agent.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxItem {
    /// A text message. `direct` is true for end-to-end encrypted direct messages.
    Message {
        from: String,
        from_name: String,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        direct: bool,
        at: u32,
    },
    /// A call to a tool this agent exposed via `expose`; answer with `reply`.
    Call {
        call_id: Uuid,
        from: String,
        from_name: String,
        tool: String,
        args: Value,
        at: u32,
        /// Seconds left before the caller gives up.
        expires_in: u64,
    },
}

/// Bounded FIFO with blocking take and a live broadcast feed.
pub struct Inbox {
    items: Mutex<VecDeque<InboxItem>>,
    notify: Notify,
    feed: broadcast::Sender<InboxItem>,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            items: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            feed: broadcast::channel(1024).0,
        }
    }
}

impl Inbox {
    pub fn push(&self, item: InboxItem) {
        {
            let mut items = self.items.lock().unwrap();
            if items.len() >= INBOX_CAPACITY {
                items.pop_front();
            }
            items.push_back(item.clone());
        }
        let _ = self.feed.send(item);
        self.notify.notify_waiters();
    }

    /// Remove up to `max` items, waiting up to `wait` for the first one.
    pub async fn take(&self, max: usize, wait: Duration) -> Vec<InboxItem> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.notify.notified();
            {
                let mut items = self.items.lock().unwrap();
                if !items.is_empty() {
                    let n = max.max(1).min(items.len());
                    return items.drain(..n).collect();
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Vec::new();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Live feed of every item as it arrives (does not consume the inbox).
    pub fn subscribe(&self) -> broadcast::Receiver<InboxItem> {
        self.feed.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> InboxItem {
        InboxItem::Message {
            from: "a".into(),
            from_name: "a".into(),
            text: text.into(),
            channel: None,
            direct: true,
            at: 0,
        }
    }

    #[tokio::test]
    async fn take_waits_for_items() {
        let inbox = std::sync::Arc::new(Inbox::default());
        let i2 = inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            i2.push(msg("hi"));
        });
        let got = inbox.take(10, Duration::from_secs(5)).await;
        assert_eq!(got.len(), 1);
        assert!(inbox.take(10, Duration::from_millis(10)).await.is_empty());
    }

    #[tokio::test]
    async fn capacity_bounded() {
        let inbox = Inbox::default();
        for i in 0..(INBOX_CAPACITY + 5) {
            inbox.push(msg(&i.to_string()));
        }
        assert_eq!(inbox.len(), INBOX_CAPACITY);
        match &inbox.take(1, Duration::ZERO).await[0] {
            InboxItem::Message { text, .. } => assert_eq!(text, "5"),
            _ => unreachable!(),
        }
    }
}
