//! Labeled-response correlation.
//!
//! `labeled-response` answers an outbound command with one tagged line, a bare `ACK`, or a `BATCH`
//! whose *opening* line carries the label; the batch's member lines and its closing line never repeat
//! it. Timeouts are entirely out of scope of that spec, so this is also where a label that never gets
//! answered is reclaimed rather than held forever.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use obby_proto::Message;

/// The reference client's labeled-response timeout: a reply that has not arrived by then is treated
/// as never coming.
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// One outstanding label, waiting for its reply or its deadline.
#[derive(Debug)]
struct Pending<T> {
    deadline_ms: u64,
    /// Set once this label's reply turns out to be a batch, so the closing line, which carries no
    /// label of its own, can still be traced back to it.
    batch_id: Option<String>,
    data: T,
}

/// Correlates outbound commands carrying a `label` tag to the message that answers them.
///
/// A label is opaque to the wire: the caller supplies whatever `T` it needs to resolve the request
/// that label was attached to, such as a channel name or an enum of what was asked. Nothing here
/// inspects `T`; it is only ever handed back to whoever registered it.
#[derive(Debug)]
pub(crate) struct Labels<T> {
    next_id: u64,
    pending: BTreeMap<String, Pending<T>>,
    /// Maps an open batch's own id to the label that opened it. Only the opening `BATCH` line carries
    /// the label; the closing line carries only the batch id, so this is how the two are reunited.
    open_batches: BTreeMap<String, String>,
}

impl<T> Labels<T> {
    /// A tracker with nothing outstanding.
    pub(crate) fn new() -> Self {
        Self {
            next_id: 0,
            pending: BTreeMap::new(),
            open_batches: BTreeMap::new(),
        }
    }

    /// A label no other outstanding request is using.
    ///
    /// Sequential rather than random, so the core stays deterministic. A bouncer is still free to
    /// rewrite the label in transit; correlation happens against this tracker's own table, never by
    /// asserting the wire value came back byte-identical.
    pub(crate) fn generate(&mut self) -> String {
        self.next_id += 1;
        alloc::format!("l{}", self.next_id)
    }

    /// Start tracking a label that was just sent, due by this monotonic instant.
    pub(crate) fn register(&mut self, label: String, deadline_ms: u64, data: T) {
        self.pending.insert(
            label,
            Pending {
                deadline_ms,
                batch_id: None,
                data,
            },
        );
    }

    /// Apply an inbound message, returning what was registered under its label once that label's
    /// response is complete.
    ///
    /// A single tagged line or a bare `ACK` resolves immediately. A labeled `BATCH` open line only
    /// arms the correlation: the registered data comes back when the matching closing line arrives,
    /// not before, since resolving on the open line would hand the caller an empty result.
    pub(crate) fn resolve(&mut self, message: &Message) -> Option<T> {
        if message.is("BATCH") {
            let marker = message.param(0)?;
            if let Some(id) = marker.strip_prefix('-') {
                let label = self.open_batches.remove(id)?;
                return self.pending.remove(&label).map(|pending| pending.data);
            }
            if let Some(id) = marker.strip_prefix('+') {
                let label = message.tag("label")?;
                let pending = self.pending.get_mut(label)?;
                pending.batch_id = Some(id.into());
                self.open_batches.insert(id.into(), label.into());
            }
            return None;
        }

        let label = message.tag("label")?;
        let pending = self.pending.remove(label)?;
        if let Some(batch_id) = &pending.batch_id {
            self.open_batches.remove(batch_id);
        }
        Some(pending.data)
    }

    /// Every label whose deadline has passed by `now_monotonic_ms`, earliest first, removed from
    /// tracking so a server that never answers cannot leak an entry forever.
    pub(crate) fn expire(&mut self, now_monotonic_ms: u64) -> Vec<T> {
        crate::timer::drain_due(&mut self.pending, now_monotonic_ms, |pending| {
            pending.deadline_ms
        })
        .into_iter()
        .map(|(_, pending)| {
            if let Some(batch_id) = &pending.batch_id {
                self.open_batches.remove(batch_id);
            }
            pending.data
        })
        .collect()
    }
}

impl<T> Default for Labels<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn line(raw: &str) -> Message {
        Message::parse(raw).expect("line should parse")
    }

    #[test]
    fn a_label_round_trips_to_the_data_registered_with_it() {
        let mut labels = Labels::new();
        let label = labels.generate();
        labels.register(label.clone(), 1_000, "whois alice".to_string());
        let reply = line(&alloc::format!(
            "@label={label} :irc.example.com 318 me alice :End of /WHOIS"
        ));
        assert_eq!(labels.resolve(&reply), Some("whois alice".to_string()));
    }

    #[test]
    fn generated_labels_are_distinct() {
        let mut labels: Labels<()> = Labels::new();
        let a = labels.generate();
        let b = labels.generate();
        assert_ne!(a, b);
    }

    #[test]
    fn a_bare_ack_resolves_a_label_with_no_visible_response() {
        let mut labels = Labels::new();
        labels.register("l1".to_string(), 1_000, "pong".to_string());
        let ack = line("@label=l1 :irc.example.com ACK");
        assert_eq!(labels.resolve(&ack), Some("pong".to_string()));
    }

    #[test]
    fn a_message_without_a_label_resolves_nothing() {
        let mut labels: Labels<()> = Labels::new();
        let msg = line(":irc.example.com PING :abc");
        assert_eq!(labels.resolve(&msg), None);
    }

    #[test]
    fn a_label_never_answered_times_out_and_is_returned() {
        let mut labels = Labels::new();
        labels.register("l1".to_string(), 1_000, "join #chan".to_string());
        assert!(labels.expire(999).is_empty());
        assert_eq!(labels.expire(1_000), vec!["join #chan".to_string()]);
    }

    #[test]
    fn a_timed_out_label_does_not_leak_and_cannot_resolve_later() {
        let mut labels = Labels::new();
        labels.register("l1".to_string(), 1_000, "join #chan".to_string());
        labels.expire(1_000);
        let late_reply = line("@label=l1 :irc.example.com 366 me #chan :End of /NAMES");
        assert_eq!(labels.resolve(&late_reply), None);
    }

    #[test]
    fn a_batch_response_only_completes_on_the_closing_line_not_the_first_member() {
        let mut labels = Labels::new();
        labels.register("l1".to_string(), 1_000, "whois alice".to_string());

        let open = line("@label=l1 :irc.example.com BATCH +b1 labeled-response");
        assert_eq!(labels.resolve(&open), None);

        let member = line("@batch=b1 :irc.example.com 311 me alice ~a host * :Alice");
        assert_eq!(labels.resolve(&member), None);

        let close = line(":irc.example.com BATCH -b1");
        assert_eq!(labels.resolve(&close), Some("whois alice".to_string()));
    }

    #[test]
    fn closing_a_batch_we_never_opened_resolves_nothing() {
        let mut labels: Labels<()> = Labels::new();
        let close = line(":irc.example.com BATCH -unknown");
        assert_eq!(labels.resolve(&close), None);
    }

    #[test]
    fn two_labels_in_flight_resolve_independently() {
        let mut labels = Labels::new();
        labels.register("l1".to_string(), 5_000, "first".to_string());
        labels.register("l2".to_string(), 5_000, "second".to_string());

        let reply2 = line("@label=l2 :irc.example.com ACK");
        assert_eq!(labels.resolve(&reply2), Some("second".to_string()));

        let reply1 = line("@label=l1 :irc.example.com ACK");
        assert_eq!(labels.resolve(&reply1), Some("first".to_string()));
    }
}
