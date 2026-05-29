use crate::session::{ContentBlock, ConversationMessage, SessionCompaction};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub snapshot_id: String,
    pub timestamp_ms: u64,
    pub message_count: usize,
    pub last_tool_name: Option<String>,
    pub last_user_text_preview: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub compaction: Option<SessionCompaction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStore {
    snapshots: Vec<SessionSnapshot>,
    max_snapshots: usize,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new(50)
    }
}

impl SnapshotStore {
    pub fn new(max_snapshots: usize) -> Self {
        Self {
            snapshots: Vec::new(),
            max_snapshots,
        }
    }

    pub fn capture(
        &mut self,
        messages: &[ConversationMessage],
        compaction: Option<&SessionCompaction>,
    ) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let snapshot_id = format!("snap-{timestamp_ms}-{seq}");

        let last_tool_name = messages.iter().rev().find_map(|msg| {
            msg.blocks.iter().find_map(|block| match block {
                ContentBlock::ToolUse { name, .. } => Some(name.clone()),
                _ => None,
            })
        });

        let last_user_text_preview = messages.iter().rev().find_map(|msg| {
            msg.blocks.iter().find_map(|block| match block {
                ContentBlock::Text { text } => {
                    let preview: String = text.chars().take(80).collect();
                    if preview.len() < text.len() {
                        Some(format!("{preview}..."))
                    } else {
                        Some(preview)
                    }
                }
                _ => None,
            })
        });

        let snapshot = SessionSnapshot {
            snapshot_id: snapshot_id.clone(),
            timestamp_ms,
            message_count: messages.len(),
            last_tool_name,
            last_user_text_preview,
            messages: messages.to_vec(),
            compaction: compaction.cloned(),
        };

        self.snapshots.push(snapshot);

        while self.snapshots.len() > self.max_snapshots {
            self.snapshots.remove(0);
        }

        snapshot_id
    }

    pub fn restore(&self, snapshot_id: &str) -> Option<&SessionSnapshot> {
        self.snapshots.iter().find(|s| s.snapshot_id == snapshot_id)
    }

    pub fn list(&self) -> &[SessionSnapshot] {
        &self.snapshots
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ConversationMessage, MessageRole};

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
        }
    }

    fn user_msg(text: &str) -> ConversationMessage {
        ConversationMessage {
            role: MessageRole::User,
            blocks: vec![text_block(text)],
            usage: None,
        }
    }

    fn assistant_msg(text: &str) -> ConversationMessage {
        ConversationMessage {
            role: MessageRole::Assistant,
            blocks: vec![text_block(text)],
            usage: None,
        }
    }

    #[test]
    fn capture_creates_snapshot_with_id() {
        let mut store = SnapshotStore::new(10);
        let messages = vec![user_msg("hello")];
        let id = store.capture(&messages, None);
        assert!(id.starts_with("snap-"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn capture_records_message_count() {
        let mut store = SnapshotStore::new(10);
        let messages = vec![user_msg("a"), assistant_msg("b"), user_msg("c")];
        store.capture(&messages, None);
        assert_eq!(store.list()[0].message_count, 3);
    }

    #[test]
    fn capture_records_user_text_preview() {
        let mut store = SnapshotStore::new(10);
        let messages = vec![user_msg("explain this code")];
        store.capture(&messages, None);
        assert_eq!(
            store.list()[0].last_user_text_preview.as_deref(),
            Some("explain this code")
        );
    }

    #[test]
    fn capture_truncates_long_preview() {
        let mut store = SnapshotStore::new(10);
        let long_text: String = "x".repeat(200);
        let messages = vec![user_msg(&long_text)];
        store.capture(&messages, None);
        let preview = store.list()[0].last_user_text_preview.as_deref().unwrap();
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= 83); // 80 chars + "..."
    }

    #[test]
    fn restore_finds_snapshot_by_id() {
        let mut store = SnapshotStore::new(10);
        let id = store.capture(&[user_msg("a")], None);
        store.capture(&[user_msg("b")], None);
        let restored = store.restore(&id);
        assert!(restored.is_some());
        assert_eq!(restored.unwrap().message_count, 1);
    }

    #[test]
    fn restore_returns_none_for_unknown_id() {
        let store = SnapshotStore::new(10);
        assert!(store.restore("snap-nonexistent").is_none());
    }

    #[test]
    fn prunes_oldest_when_exceeding_max() {
        let mut store = SnapshotStore::new(3);
        let id1 = store.capture(&[user_msg("a")], None);
        store.capture(&[user_msg("b")], None);
        store.capture(&[user_msg("c")], None);
        store.capture(&[user_msg("d")], None);
        assert_eq!(store.len(), 3);
        assert!(store.restore(&id1).is_none()); // oldest was pruned
    }

    #[test]
    fn empty_store_is_empty() {
        let store = SnapshotStore::new(10);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn list_returns_all_snapshots() {
        let mut store = SnapshotStore::new(10);
        store.capture(&[user_msg("a")], None);
        store.capture(&[user_msg("b")], None);
        assert_eq!(store.list().len(), 2);
    }
}
