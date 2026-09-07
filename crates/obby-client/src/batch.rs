//! `BATCH` framing: nesting, interleaving, and per-batch message buffering.
//!
//! This module only ever sees `BATCH` lines and the messages that carry a `batch` tag; it holds
//! no reference to anything else the client knows, so a batch can be assembled and torn down the
//! same way whether it wraps `chathistory`, a netsplit, or a type this crate has never heard of.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use obby_proto::Message;

/// How many messages a single batch buffers before it is marked overflowed.
///
/// A server that opens a batch and never closes it, whether by bug or by malice, would otherwise
/// let this connection's memory grow without bound; this is the ceiling on any one batch's damage.
pub(crate) const DEFAULT_MAX_BUFFERED: usize = 1024;

/// How many batches may be open at once.
///
/// The per-batch bound says nothing about how many batches there are, so a server that opens
/// references and never closes them would otherwise grow this map without bound.
pub(crate) const DEFAULT_MAX_OPEN: usize = 64;

/// One batch that finished, with everything it collected while it was open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClosedBatch {
    /// The batch type from its `BATCH +ref type` line, kept even when we don't recognise it.
    pub(crate) kind: String,
    /// The parameters that followed the type on the open line.
    pub(crate) params: Vec<String>,
    /// The reference of the batch that held this one open, when it was nested.
    pub(crate) parent: Option<String>,
    /// The messages routed into this batch, in arrival order, deduplicated by `msgid`.
    pub(crate) messages: Vec<Message>,
    /// True once this batch's buffer hit [`DEFAULT_MAX_BUFFERED`] (or the configured bound), so
    /// `messages` is a prefix of what the server actually sent rather than the whole batch.
    pub(crate) overflowed: bool,
}

/// A batch still open, tracked from its `BATCH +ref type` line onward.
#[derive(Debug, Clone)]
struct OpenBatch {
    kind: String,
    params: Vec<String>,
    parent: Option<String>,
    messages: Vec<Message>,
    /// Every `msgid` already buffered, so a server repeating a line within one batch (a retry, an
    /// overlapping history page) is not stored twice. This never checks against anything outside
    /// the batch: that comparison belongs to whoever merges the closed batch into the model.
    seen: BTreeSet<String>,
    overflowed: bool,
}

impl OpenBatch {
    fn push(&mut self, message: Message, max_buffered: usize) {
        if self.overflowed {
            return;
        }
        let msgid = message.tag("msgid").map(ToOwned::to_owned);
        if let Some(id) = &msgid
            && self.seen.contains(id.as_str())
        {
            return;
        }
        if self.messages.len() >= max_buffered {
            self.overflowed = true;
            return;
        }
        if let Some(id) = msgid {
            self.seen.insert(id);
        }
        self.messages.push(message);
    }

    fn into_closed(self) -> ClosedBatch {
        ClosedBatch {
            kind: self.kind,
            params: self.params,
            parent: self.parent,
            messages: self.messages,
            overflowed: self.overflowed,
        }
    }
}

/// Open `BATCH` framing for one connection.
///
/// Batches are keyed by their reference tag rather than tracked as a single "current batch",
/// because sibling batches legally interleave on the wire: a line tagged for batch `1` can arrive
/// between two lines of batch `2`.
#[derive(Debug, Clone)]
pub(crate) struct Batches {
    by_reference: BTreeMap<String, OpenBatch>,
    max_buffered: usize,
    max_open: usize,
}

impl Batches {
    /// A tracker with no batches open, bounding each one to [`DEFAULT_MAX_BUFFERED`] messages.
    pub(crate) fn new() -> Self {
        Self::with_max_buffered(DEFAULT_MAX_BUFFERED)
    }

    /// A tracker with no batches open, bounding each one to `max_buffered` messages.
    pub(crate) fn with_max_buffered(max_buffered: usize) -> Self {
        Self {
            by_reference: BTreeMap::new(),
            max_buffered,
            max_open: DEFAULT_MAX_OPEN,
        }
    }

    /// Handle a `BATCH +<ref> <type> [params...]` line, opening a new batch.
    ///
    /// A `batch` tag on this line names the outer batch this one nests inside, per the nesting
    /// rule that both the open and the close of an inner batch carry the outer's tag. A line that
    /// does not carry a `+ref` and a type is not a valid open and is silently ignored, since there
    /// is nothing safe to track from it.
    pub(crate) fn open(&mut self, message: &Message) {
        let Some(reference) = message.param(0).and_then(|marker| marker.strip_prefix('+')) else {
            return;
        };
        let Some(kind) = message.param(1) else {
            return;
        };
        // reusing an open reference would drop whatever it had already collected, delivered neither
        // live nor as history, so the first batch under a name keeps it
        if self.by_reference.contains_key(reference) {
            return;
        }
        if self.by_reference.len() >= self.max_open {
            return;
        }
        let params = message.params.get(2..).unwrap_or_default().to_vec();
        let parent = message.tag("batch").map(ToOwned::to_owned);
        self.by_reference.insert(
            reference.to_owned(),
            OpenBatch {
                kind: kind.to_owned(),
                params,
                parent,
                messages: Vec::new(),
                seen: BTreeSet::new(),
                overflowed: false,
            },
        );
    }

    /// Handle a `BATCH -<ref>` line, closing the batch and returning what it collected.
    ///
    /// Returns `None` for a reference we never opened. An unmatched close is a protocol violation
    /// on the server's part, and the defensive answer is to treat it as a no-op, not a panic.
    pub(crate) fn close(&mut self, message: &Message) -> Option<ClosedBatch> {
        let reference = message.param(0)?.strip_prefix('-')?;
        self.by_reference
            .remove(reference)
            .map(OpenBatch::into_closed)
    }

    /// Route one message into the batch its `batch` tag names, if that batch is still open.
    ///
    /// Returns the message back when it carries no `batch` tag, or names a reference we are not
    /// holding open, so the caller can fall through to handling it as an ordinary live message.
    pub(crate) fn route(&mut self, message: Message) -> Option<Message> {
        let reference = message.tag("batch").map(ToOwned::to_owned);
        let Some(reference) = reference else {
            return Some(message);
        };
        let max_buffered = self.max_buffered;
        match self.by_reference.get_mut(&reference) {
            Some(batch) => {
                batch.push(message, max_buffered);
                None
            }
            None => Some(message),
        }
    }

    /// True when `reference` names an open batch of type `kind`, or any batch that holds it open
    /// (directly or transitively) does.
    ///
    /// A message's `batch` tag names its innermost batch, which may itself be nested inside
    /// others, such as a `netsplit` batch replayed inside a `chathistory` one; a caller asking
    /// whether a message is replayed history needs the whole ancestor chain, not just the
    /// innermost link.
    pub(crate) fn is_within(&self, reference: &str, kind: &str) -> bool {
        let mut current = self.by_reference.get(reference);
        let mut steps = 0;
        while let Some(batch) = current {
            if batch.kind == kind {
                return true;
            }
            // a server could name two batches as each other's parent; no legitimate chain visits
            // more ancestors than there are batches open, so this bounds a cycle instead of
            // looping forever
            steps += 1;
            if steps > self.by_reference.len() {
                return false;
            }
            current = batch
                .parent
                .as_deref()
                .and_then(|parent| self.by_reference.get(parent));
        }
        false
    }

    /// Drop every open batch outright, for a connection that just died.
    ///
    /// Nothing an unfinished batch was assembling is worth keeping: a reconnect asks the server
    /// for whatever it needs again and every batch starts fresh.
    pub(crate) fn drop_all(&mut self) {
        self.by_reference.clear();
    }
}

impl Default for Batches {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obby_proto::Tag;

    fn open_line(reference: &str, kind: &str, params: &[&str]) -> Message {
        let mut wire = Vec::from([alloc::format!("+{reference}"), kind.to_owned()]);
        wire.extend(params.iter().map(|param| (*param).to_owned()));
        Message::new("BATCH", wire)
    }

    fn close_line(reference: &str) -> Message {
        Message::new("BATCH", [alloc::format!("-{reference}")])
    }

    fn tagged(mut message: Message, key: &str, value: &str) -> Message {
        message.tags.set(Tag::new(key, value));
        message
    }

    fn privmsg(target: &str, body: &str) -> Message {
        Message::new("PRIVMSG", [target, body])
    }

    #[test]
    fn reopening_a_reference_keeps_what_it_already_collected() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &["#chan"]));
        batches.route(tagged(privmsg("#chan", "one"), "batch", "a"));
        batches.open(&open_line("a", "netjoin", &[]));
        let closed = batches.close(&close_line("a")).expect("the batch is open");
        assert_eq!(closed.kind, "chathistory");
        assert_eq!(closed.messages.len(), 1);
    }

    #[test]
    fn a_server_cannot_open_unboundedly_many_batches() {
        let mut batches = Batches::new();
        for index in 0..DEFAULT_MAX_OPEN + 10 {
            batches.open(&open_line(&alloc::format!("b{index}"), "chathistory", &[]));
        }
        assert!(batches.close(&close_line("b0")).is_some());
        assert!(
            batches
                .close(&close_line(&alloc::format!("b{DEFAULT_MAX_OPEN}")))
                .is_none()
        );
    }

    #[test]
    fn opens_and_closes_a_batch_returning_its_messages_in_order() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &["#chan"]));
        assert!(
            batches
                .route(tagged(privmsg("#chan", "one"), "batch", "a"))
                .is_none()
        );
        assert!(
            batches
                .route(tagged(privmsg("#chan", "two"), "batch", "a"))
                .is_none()
        );

        let closed = batches.close(&close_line("a")).expect("batch was open");
        assert_eq!(closed.kind, "chathistory");
        assert_eq!(closed.messages.len(), 2);
        assert_eq!(closed.messages[0].trailing(), Some("one"));
        assert_eq!(closed.messages[1].trailing(), Some("two"));
    }

    #[test]
    fn a_nested_batch_is_within_its_ancestor_at_any_depth() {
        let mut batches = Batches::new();
        batches.open(&open_line("outer", "chathistory", &[]));
        batches.open(&tagged(
            open_line("inner", "netsplit", &[]),
            "batch",
            "outer",
        ));

        assert!(batches.is_within("inner", "netsplit"));
        assert!(batches.is_within("inner", "chathistory"));
        assert!(!batches.is_within("outer", "netsplit"));
        assert!(!batches.is_within("nonexistent", "chathistory"));
    }

    #[test]
    fn a_message_with_no_batch_tag_is_returned_unchanged() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &[]));
        let message = privmsg("#chan", "hello");
        let returned = batches
            .route(message.clone())
            .expect("an unbatched message is handed back");
        assert_eq!(returned, message);
    }

    #[test]
    fn closing_a_reference_never_opened_returns_none() {
        let mut batches = Batches::new();
        assert!(batches.close(&close_line("nonexistent")).is_none());
    }

    #[test]
    fn a_batch_left_open_when_the_connection_drops_is_forgotten() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &[]));
        batches.drop_all();
        assert!(batches.close(&close_line("a")).is_none());
    }

    #[test]
    fn deduplicates_by_msgid_within_one_batch() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &[]));
        let first = tagged(
            tagged(privmsg("#chan", "hello"), "msgid", "m1"),
            "batch",
            "a",
        );
        let repeat = first.clone();
        batches.route(first);
        batches.route(repeat);

        let closed = batches.close(&close_line("a")).expect("batch was open");
        assert_eq!(closed.messages.len(), 1);
    }

    #[test]
    fn enforces_the_buffer_bound_and_flags_the_overflow() {
        let mut batches = Batches::with_max_buffered(2);
        batches.open(&open_line("a", "chathistory", &[]));
        for id in ["m1", "m2", "m3"] {
            batches.route(tagged(
                tagged(privmsg("#chan", id), "msgid", id),
                "batch",
                "a",
            ));
        }

        let closed = batches.close(&close_line("a")).expect("batch was open");
        assert_eq!(closed.messages.len(), 2);
        assert!(closed.overflowed);
    }

    #[test]
    fn preserves_the_parameters_from_the_open_line() {
        let mut batches = Batches::new();
        batches.open(&open_line("a", "chathistory", &["#chan", "extra"]));
        let closed = batches.close(&close_line("a")).expect("batch was open");
        assert_eq!(closed.params, ["#chan".to_owned(), "extra".to_owned()]);
    }
}
