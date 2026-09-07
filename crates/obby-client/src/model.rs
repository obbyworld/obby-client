//! The client model.
//!
//! Everything the connection knows: who we are, the channels we are in and who is in them, the
//! private conversations, and the messages. An app renders this and keeps no second copy.

use alloc::collections::{BTreeMap, btree_map};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use obby_proto::{CaseFolded, Casemapping, Prefix};

/// How many messages one channel or conversation keeps before the oldest are dropped.
///
/// The cap applies the same way to live traffic and to history backfill. The reference client trims
/// only when merging history, so how much scrollback a channel holds depends on whether it ever ran
/// a history request.
pub const DEFAULT_RETENTION: usize = 5000;

/// Where a message is ordered and how it is found again.
///
/// Ordering is by the server's timestamp, with a monotonic sequence number breaking ties. A tie
/// broken by arrival order alone is what the reference client relies on, and it only holds there
/// because of an incidental property of the sort it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "MessageOrder"))]
pub struct MessageKey {
    /// Milliseconds since the epoch, from `server-time` when the server sent one.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub time_ms: u64,
    /// Assigned in arrival order, unique for the life of the connection.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
}

/// What kind of thing happened.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum MessageKind {
    /// An ordinary message.
    Privmsg,
    /// A notice, which by convention a client must not auto-reply to.
    Notice,
    /// A CTCP, carrying the command that was requested.
    Ctcp {
        /// The CTCP command, uppercased, such as `ACTION`.
        command: String,
    },
    /// A bodiless message that exists only to carry tags.
    Tagmsg,
    /// Someone joined.
    Join,
    /// Someone left the channel.
    Part,
    /// Someone left the network.
    Quit,
    /// Someone was removed by an operator.
    Kick {
        /// Who was removed.
        target: String,
    },
    /// Someone changed nick.
    Nick {
        /// What they changed it to.
        new_nick: String,
    },
    /// The topic changed.
    Topic,
    /// Modes changed.
    Mode,
}

/// One message, as the model holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "ChatMessage"))]
pub struct Message {
    /// Where it sits in the log.
    pub key: MessageKey,
    /// The network-unique id, when the server assigned one.
    pub msgid: Option<String>,
    /// Who sent it, as they spell their own nick.
    pub sender: String,
    /// What kind of event it is.
    pub kind: MessageKind,
    /// The body, empty for events that have none.
    pub text: String,
    /// The account the sender was logged in as, from `account-tag`.
    pub account: Option<String>,
    /// True when this arrived inside a history batch rather than live.
    pub historical: bool,
    /// True when this is our own message coming back through `echo-message`.
    pub own: bool,
    /// The message this one replies to, from the `+reply` tag.
    ///
    /// The server is inconsistent about the spelling and sends `+reply` from one module and
    /// `+draft/reply` from another, so both are accepted on the way in.
    pub reply_to: Option<String>,
    /// Reactions, keyed by the emoji, holding who reacted.
    pub reactions: BTreeMap<String, Vec<String>>,
    /// A preview of the first link in this message, when the server built one.
    #[cfg(feature = "obby")]
    pub link_preview: Option<crate::extensions::LinkPreview>,
    /// True once the message was redacted. The original is kept, because throwing it away leaves no
    /// way to show who redacted what.
    pub redacted: bool,
}

impl Message {
    /// A message with only what every kind carries.
    pub fn new(key: MessageKey, sender: impl Into<String>, kind: MessageKind) -> Self {
        Self {
            key,
            msgid: None,
            sender: sender.into(),
            kind,
            text: String::new(),
            account: None,
            historical: false,
            own: false,
            reply_to: None,
            reactions: BTreeMap::new(),
            #[cfg(feature = "obby")]
            link_preview: None,
            redacted: false,
        }
    }
}

/// The messages of one channel or conversation, ordered and bounded.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "MessageLog"))]
pub struct Log {
    #[cfg_attr(feature = "serde", serde(with = "messages_as_list"))]
    #[cfg_attr(feature = "ts", ts(as = "Vec<Message>"))]
    messages: BTreeMap<MessageKey, Message>,
    by_msgid: BTreeMap<String, MessageKey>,
    retention: usize,
}

/// Carry the message log as a list rather than a map.
///
/// The map is keyed by a [`MessageKey`], and JSON has no way to spell a structured object key, so
/// every binding that serialises the model would otherwise fail on the first channel that has said
/// anything. Each message already carries its own key, so the map rebuilds from the list exactly.
#[cfg(feature = "serde")]
mod messages_as_list {
    use super::{BTreeMap, Message, MessageKey};
    use alloc::vec::Vec;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        messages: &BTreeMap<MessageKey, Message>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        messages.values().collect::<Vec<_>>().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<MessageKey, Message>, D::Error> {
        Ok(Vec::<Message>::deserialize(deserializer)?
            .into_iter()
            .map(|message| (message.key, message))
            .collect())
    }
}

impl Default for Log {
    fn default() -> Self {
        Self::with_retention(DEFAULT_RETENTION)
    }
}

impl Log {
    /// An empty log holding at most this many messages.
    pub fn with_retention(retention: usize) -> Self {
        Self {
            messages: BTreeMap::new(),
            by_msgid: BTreeMap::new(),
            retention: retention.max(1),
        }
    }

    /// Insert a message unless we already have it, returning whether it was new.
    ///
    /// A message with an id is deduplicated by that id. One without is compared against the
    /// messages sharing its exact timestamp, which is enough to catch a replay while staying
    /// bounded: an unbounded content scan would be the only alternative.
    pub fn insert(&mut self, message: Message) -> bool {
        if let Some(msgid) = &message.msgid {
            if self.by_msgid.contains_key(msgid) {
                return false;
            }
        } else if self.has_twin(&message) {
            return false;
        }
        if let Some(msgid) = &message.msgid {
            self.by_msgid.insert(msgid.clone(), message.key);
        }
        self.messages.insert(message.key, message);
        self.trim();
        true
    }

    fn has_twin(&self, candidate: &Message) -> bool {
        let same_instant = MessageKey {
            time_ms: candidate.key.time_ms,
            seq: 0,
        }..MessageKey {
            time_ms: candidate.key.time_ms.saturating_add(1),
            seq: 0,
        };
        self.messages.range(same_instant).any(|(_, held)| {
            held.sender == candidate.sender
                && held.text == candidate.text
                && held.kind == candidate.kind
        })
    }

    fn trim(&mut self) {
        while self.messages.len() > self.retention {
            let Some((_, dropped)) = self.messages.pop_first() else {
                return;
            };
            if let Some(msgid) = dropped.msgid {
                self.by_msgid.remove(&msgid);
            }
        }
    }

    /// True when a message with this id is already held.
    pub fn contains(&self, msgid: &str) -> bool {
        self.by_msgid.contains_key(msgid)
    }

    /// Look one up by its network id.
    pub fn get(&self, msgid: &str) -> Option<&Message> {
        self.messages.get(self.by_msgid.get(msgid)?)
    }

    /// Borrow one mutably by its network id, to attach a reaction or mark it redacted.
    pub fn get_mut(&mut self, msgid: &str) -> Option<&mut Message> {
        let key = *self.by_msgid.get(msgid)?;
        self.messages.get_mut(&key)
    }

    /// Every message, oldest first.
    pub fn iter(&self) -> btree_map::Values<'_, MessageKey, Message> {
        self.messages.values()
    }

    /// The most recent message.
    pub fn last(&self) -> Option<&Message> {
        self.messages.last_key_value().map(|(_, message)| message)
    }

    /// The oldest message held, which is where a history request should resume from.
    pub fn first(&self) -> Option<&Message> {
        self.messages.first_key_value().map(|(_, message)| message)
    }

    /// How many are held.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

impl<'a> IntoIterator for &'a Log {
    type Item = &'a Message;
    type IntoIter = btree_map::Values<'a, MessageKey, Message>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// What one member holds in one channel.
///
/// Only the channel-specific part. Who they are, what account they hold and whether they are away
/// are the same everywhere, so they live once on [`Person`] rather than being copied into every
/// channel they are in and drifting apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Membership {
    /// The prefix characters they hold here, highest rank first.
    pub prefixes: String,
}

impl Membership {
    /// The highest prefix they hold, which is what a compact member list shows.
    pub fn top_prefix(&self) -> Option<char> {
        self.prefixes.chars().next()
    }

    /// Add a prefix, keeping the list in the server's rank order.
    pub fn grant(&mut self, prefix: char, order: &Prefix) {
        if self.prefixes.contains(prefix) {
            return;
        }
        self.prefixes.push(prefix);
        let mut ranked: Vec<char> = self.prefixes.chars().collect();
        ranked.sort_by_key(|c| order.rank(*c).unwrap_or(usize::MAX));
        self.prefixes = ranked.into_iter().collect();
    }

    /// Remove a prefix.
    pub fn revoke(&mut self, prefix: char) {
        self.prefixes.retain(|c| c != prefix);
    }
}

/// Someone we know about, held once however many channels we share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Person {
    /// Their nick, as they spell it.
    pub nick: String,
    /// The account they are logged in as.
    pub account: Option<String>,
    /// Their away message, when they are away.
    pub away: Option<String>,
    /// True when the server marks them as a bot.
    pub bot: bool,
    /// Their username, from WHO or a hostmask.
    pub username: Option<String>,
    /// Their host, from WHO or a hostmask.
    pub host: Option<String>,
    /// Their realname, from WHO.
    pub realname: Option<String>,
    /// True when the server marks them as an operator.
    pub operator: bool,
    /// Metadata the server holds, such as `display-name`, `color` and `avatar`.
    ///
    /// These are plain `draft/metadata-2` keys with no vendor prefix, despite everything else Obby
    /// adds being namespaced.
    pub metadata: BTreeMap<String, String>,
}

/// A channel we are in.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Channel {
    /// The name as the server spells it.
    pub name: String,
    /// The topic, when one is set.
    pub topic: Option<String>,
    /// Who set the topic and when.
    pub topic_by: Option<String>,
    /// Channel modes currently set, with their arguments.
    pub modes: BTreeMap<char, Option<String>>,
    /// Who is in it.
    pub members: BTreeMap<CaseFolded, Membership>,
    /// What was said.
    pub log: Log,
    /// Messages since the last read marker.
    pub unread: u32,
    /// Unread messages that mention us.
    pub mentions: u32,
    /// Metadata the server holds, such as `display-name`, `color`, `avatar` and `bot`.
    ///
    /// These are plain `draft/metadata-2` keys with no vendor prefix, despite everything else Obby
    /// adds being namespaced.
    pub metadata: BTreeMap<String, String>,
    /// The read marker timestamp the server last confirmed.
    pub read_marker: Option<String>,
    /// Who is composing a message here right now.
    pub typing: alloc::collections::BTreeSet<CaseFolded>,
    /// Modes by their name rather than their letter, from `draft/named-modes`.
    ///
    /// A letter means nothing without the server telling you what it does, and two servers spell
    /// the same feature differently. The names are stable, so this is what a user interface should
    /// show and what a setting should be keyed by.
    pub named_modes: BTreeMap<String, Option<String>>,
}

/// A private conversation with one other person.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "Conversation"))]
pub struct Conversation {
    /// Their nick, as they spell it.
    pub nick: String,
    /// What was said.
    pub log: Log,
    /// Messages since the last read marker.
    pub unread: u32,
    /// The read marker timestamp the server last confirmed.
    pub read_marker: Option<String>,
    /// Who is composing a message here right now.
    pub typing: alloc::collections::BTreeSet<CaseFolded>,
}

/// Who we are on this connection.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "LocalUser"))]
pub struct Me {
    /// Our current nick.
    pub nick: String,
    /// The account we authenticated as.
    pub account: Option<String>,
    /// Our user modes.
    pub modes: String,
    /// Metadata the server holds, such as `display-name`, `color`, `avatar` and `bot`.
    ///
    /// These are plain `draft/metadata-2` keys with no vendor prefix, despite everything else Obby
    /// adds being namespaced.
    pub metadata: BTreeMap<String, String>,
    /// Our away message, when we are away.
    pub away: Option<String>,
}

/// Everything the connection knows.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Model {
    /// Who we are.
    pub me: Me,
    channels: BTreeMap<CaseFolded, Channel>,
    conversations: BTreeMap<CaseFolded, Conversation>,
    people: BTreeMap<CaseFolded, Person>,
    retention: usize,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    next_seq: u64,
}

impl Default for Model {
    fn default() -> Self {
        Self::with_retention(DEFAULT_RETENTION)
    }
}

impl Model {
    /// An empty model with this retention limit per target.
    pub fn with_retention(retention: usize) -> Self {
        Self {
            me: Me::default(),
            channels: BTreeMap::new(),
            conversations: BTreeMap::new(),
            people: BTreeMap::new(),
            retention: retention.max(1),
            next_seq: 0,
        }
    }

    /// The next sequence number, which breaks timestamp ties in arrival order.
    pub fn next_key(&mut self, time_ms: u64) -> MessageKey {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        MessageKey { time_ms, seq }
    }

    /// A channel by name, folded with the server's casemapping.
    pub fn channel(&self, key: &CaseFolded) -> Option<&Channel> {
        self.channels.get(key)
    }

    /// A channel, created if we have not seen it.
    pub fn channel_mut(&mut self, key: CaseFolded, name: &str) -> &mut Channel {
        let retention = self.retention;
        self.channels.entry(key).or_insert_with(|| Channel {
            name: name.to_string(),
            log: Log::with_retention(retention),
            ..Channel::default()
        })
    }

    /// Every channel, for changing what is in them.
    pub fn channels_mut(&mut self) -> btree_map::IterMut<'_, CaseFolded, Channel> {
        self.channels.iter_mut()
    }

    /// A channel we already know, for changing what is in it.
    pub fn existing_channel_mut(&mut self, key: &CaseFolded) -> Option<&mut Channel> {
        self.channels.get_mut(key)
    }

    /// Forget a channel, which is what leaving one means.
    pub fn remove_channel(&mut self, key: &CaseFolded) -> Option<Channel> {
        self.channels.remove(key)
    }

    /// Every channel, in folded name order.
    pub fn channels(&self) -> btree_map::Iter<'_, CaseFolded, Channel> {
        self.channels.iter()
    }

    /// A private conversation.
    pub fn conversation(&self, key: &CaseFolded) -> Option<&Conversation> {
        self.conversations.get(key)
    }

    /// A private conversation, created if we have not seen it.
    pub fn conversation_mut(&mut self, key: CaseFolded, nick: &str) -> &mut Conversation {
        let retention = self.retention;
        self.conversations
            .entry(key)
            .or_insert_with(|| Conversation {
                nick: nick.to_string(),
                log: Log::with_retention(retention),
                ..Conversation::default()
            })
    }

    /// Record that someone started or stopped composing a message somewhere.
    ///
    /// Returns true when this changed anything, so a caller does not report a state that already
    /// held. A typing indicator repeats on a timer, and re-reporting each repeat makes it flicker.
    pub fn set_typing(&mut self, target: &CaseFolded, who: CaseFolded, typing: bool) -> bool {
        let set = match self.channels.get_mut(target) {
            Some(channel) => &mut channel.typing,
            None => match self.conversations.get_mut(target) {
                Some(query) => &mut query.typing,
                None => return false,
            },
        };
        if typing {
            set.insert(who)
        } else {
            set.remove(&who)
        }
    }

    /// Someone we know about.
    pub fn person(&self, key: &CaseFolded) -> Option<&Person> {
        self.people.get(key)
    }

    /// Someone we know about, remembered from now on if we did not already.
    pub fn person_mut(&mut self, key: CaseFolded, nick: &str) -> &mut Person {
        self.people.entry(key).or_insert_with(|| Person {
            nick: nick.to_string(),
            ..Person::default()
        })
    }

    /// Someone we already know, for changing what we hold about them.
    pub fn existing_person_mut(&mut self, key: &CaseFolded) -> Option<&mut Person> {
        self.people.get_mut(key)
    }

    /// A private conversation we already know, for changing what is in it.
    pub fn existing_conversation_mut(&mut self, key: &CaseFolded) -> Option<&mut Conversation> {
        self.conversations.get_mut(key)
    }

    /// Every private conversation.
    pub fn conversations(&self) -> btree_map::Iter<'_, CaseFolded, Conversation> {
        self.conversations.iter()
    }

    /// Rename someone everywhere they appear, which is what a NICK means.
    ///
    /// Membership is keyed by the folded nick, so a rename has to move every entry rather than edit
    /// one field, and a fold that only lowercases ASCII would leave duplicates behind on any server
    /// that folds the bracket alphabet.
    pub fn rename(&mut self, casemapping: Casemapping, from: &str, to: &str) {
        let (old, new) = (casemapping.fold(from), casemapping.fold(to));
        for channel in self.channels.values_mut() {
            if let Some(membership) = channel.members.remove(&old) {
                channel.members.insert(new.clone(), membership);
            }
        }
        if let Some(mut query) = self.conversations.remove(&old) {
            query.nick = to.to_string();
            self.conversations.insert(new.clone(), query);
        }
        if let Some(mut person) = self.people.remove(&old) {
            person.nick = to.to_string();
            self.people.insert(new, person);
        }
        if casemapping.eq(&self.me.nick, from) {
            self.me.nick = to.to_string();
        }
    }

    /// Forget anyone we no longer share a channel or a conversation with.
    ///
    /// Without this a long session accumulates one permanent record per nick it has ever seen,
    /// which a busy channel supplies for free.
    pub fn forget_strangers(&mut self) {
        let mut keep: alloc::collections::BTreeSet<CaseFolded> =
            self.conversations.keys().cloned().collect();
        for channel in self.channels.values() {
            keep.extend(channel.members.keys().cloned());
        }
        let me = self.me.nick.clone();
        self.people
            .retain(|key, person| keep.contains(key) || person.nick == me);
    }

    /// Remove someone from every channel, which is what a QUIT means. Returns where they were.
    pub fn remove_everywhere(&mut self, who: &CaseFolded) -> Vec<CaseFolded> {
        let mut left = Vec::new();
        for (key, channel) in &mut self.channels {
            if channel.members.remove(who).is_some() {
                left.push(key.clone());
            }
        }
        left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(seq: u64, time_ms: u64, sender: &str, text: &str) -> Message {
        let mut message = Message::new(MessageKey { time_ms, seq }, sender, MessageKind::Privmsg);
        message.text = text.to_string();
        message
    }

    fn with_id(mut message: Message, msgid: &str) -> Message {
        message.msgid = Some(msgid.to_string());
        message
    }

    #[test]
    fn orders_by_timestamp_not_by_arrival() {
        let mut log = Log::default();
        assert!(log.insert(message(0, 200, "a", "second")));
        assert!(log.insert(message(1, 100, "a", "first")));
        let texts: Vec<&str> = log.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            ["first", "second"],
            "a backfilled message sorts into place"
        );
    }

    #[test]
    fn breaks_a_timestamp_tie_by_arrival() {
        let mut log = Log::default();
        log.insert(message(0, 100, "a", "one"));
        log.insert(message(1, 100, "a", "two"));
        log.insert(message(2, 100, "a", "three"));
        let texts: Vec<&str> = log.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["one", "two", "three"]);
    }

    #[test]
    fn refuses_a_message_it_already_has_by_id() {
        let mut log = Log::default();
        assert!(log.insert(with_id(message(0, 100, "a", "hi"), "x1")));
        assert!(
            !log.insert(with_id(message(1, 100, "a", "hi"), "x1")),
            "a bouncer replaying the same msgid must not double up"
        );
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn refuses_an_identical_message_with_no_id() {
        let mut log = Log::default();
        assert!(log.insert(message(0, 100, "a", "hi")));
        assert!(!log.insert(message(1, 100, "a", "hi")));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn keeps_the_same_text_at_a_different_instant() {
        let mut log = Log::default();
        assert!(log.insert(message(0, 100, "a", "hi")));
        assert!(
            log.insert(message(1, 101, "a", "hi")),
            "saying it twice is not a duplicate"
        );
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn keeps_the_same_text_from_a_different_sender() {
        let mut log = Log::default();
        assert!(log.insert(message(0, 100, "a", "hi")));
        assert!(log.insert(message(1, 100, "b", "hi")));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn trims_the_oldest_once_it_is_full() {
        let mut log = Log::with_retention(3);
        for i in 0..5 {
            log.insert(message(i, 100 + i, "a", "x"));
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.first().map(|m| m.key.time_ms), Some(102));
        assert_eq!(log.last().map(|m| m.key.time_ms), Some(104));
    }

    #[test]
    fn trimming_forgets_the_id_index_too() {
        let mut log = Log::with_retention(2);
        log.insert(with_id(message(0, 100, "a", "x"), "old"));
        log.insert(with_id(message(1, 101, "a", "x"), "mid"));
        log.insert(with_id(message(2, 102, "a", "x"), "new"));
        assert!(
            !log.contains("old"),
            "a dropped message must not linger in the index"
        );
        assert!(log.contains("new"));
        assert_eq!(log.get("old"), None);
    }

    #[test]
    fn the_cap_applies_to_backfill_the_same_way() {
        let mut log = Log::with_retention(2);
        log.insert(message(0, 300, "a", "live"));
        log.insert(message(1, 100, "a", "old"));
        log.insert(message(2, 200, "a", "older"));
        assert_eq!(log.len(), 2);
        let texts: Vec<&str> = log.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            ["older", "live"],
            "the oldest goes, whichever way it arrived"
        );
    }

    #[test]
    fn finds_a_message_again_to_react_to_it() {
        let mut log = Log::default();
        log.insert(with_id(message(0, 100, "a", "hi"), "x1"));
        let found = log.get_mut("x1").expect("the message is there");
        found
            .reactions
            .entry("👍".to_string())
            .or_default()
            .push("b".to_string());
        assert_eq!(log.get("x1").map(|m| m.reactions.len()), Some(1));
    }

    #[test]
    fn ranks_prefixes_in_the_servers_order_not_the_order_granted() {
        let order = Prefix::parse("(qaohv)~&@%+").expect("valid prefix");
        let mut member = Membership::default();
        member.grant('+', &order);
        member.grant('~', &order);
        member.grant('@', &order);
        assert_eq!(member.prefixes, "~@+");
        assert_eq!(member.top_prefix(), Some('~'));
        member.revoke('~');
        assert_eq!(member.top_prefix(), Some('@'));
    }

    #[test]
    fn granting_the_same_prefix_twice_changes_nothing() {
        let order = Prefix::default();
        let mut member = Membership::default();
        member.grant('@', &order);
        member.grant('@', &order);
        assert_eq!(member.prefixes, "@");
    }

    #[test]
    fn a_rename_moves_the_member_rather_than_leaving_a_twin() {
        let map = Casemapping::Rfc1459;
        let mut model = Model::default();
        model
            .channel_mut(map.fold("#obby"), "#obby")
            .members
            .insert(map.fold("[nick]"), Membership::default());
        model.person_mut(map.fold("[nick]"), "[nick]");

        model.rename(map, "[nick]", "{NICK}2");

        let channel = model.channel(&map.fold("#obby")).expect("channel");
        assert_eq!(
            channel.members.len(),
            1,
            "folding {{}} onto [] must not leave two entries"
        );
        assert!(channel.members.contains_key(&map.fold("{nick}2")));
        assert_eq!(
            model.person(&map.fold("{nick}2")).map(|p| p.nick.as_str()),
            Some("{NICK}2"),
            "the person is renamed once, not once per channel"
        );
    }

    #[test]
    fn a_rename_of_ourselves_updates_who_we_are() {
        let map = Casemapping::Rfc1459;
        let mut model = Model::default();
        model.me.nick = "me".to_string();
        model.rename(map, "ME", "you");
        assert_eq!(
            model.me.nick, "you",
            "the server may echo our nick in any case"
        );
    }

    #[test]
    fn a_quit_reports_every_channel_they_were_in() {
        let map = Casemapping::Rfc1459;
        let mut model = Model::default();
        for name in ["#a", "#b", "#c"] {
            model
                .channel_mut(map.fold(name), name)
                .members
                .insert(map.fold("bob"), Membership::default());
        }
        model
            .channel_mut(map.fold("#c"), "#c")
            .members
            .remove(&map.fold("bob"));

        let left = model.remove_everywhere(&map.fold("bob"));
        assert_eq!(left, [map.fold("#a"), map.fold("#b")]);
    }

    #[test]
    fn retention_reaches_every_target_it_creates() {
        let map = Casemapping::Rfc1459;
        let mut model = Model::with_retention(2);
        let channel = model.channel_mut(map.fold("#a"), "#a");
        for i in 0..4 {
            channel.log.insert(message(i, 100 + i, "a", "x"));
        }
        assert_eq!(channel.log.len(), 2);

        let query = model.conversation_mut(map.fold("bob"), "bob");
        for i in 0..4 {
            query.log.insert(message(i, 100 + i, "bob", "x"));
        }
        assert_eq!(query.log.len(), 2);
    }

    #[test]
    fn a_default_model_keeps_more_than_one_message() {
        let map = Casemapping::Rfc1459;
        let mut model = Model::default();
        let channel = model.channel_mut(map.fold("#a"), "#a");
        for i in 0..10 {
            channel.log.insert(message(i, 100 + i, "a", "x"));
        }
        assert_eq!(channel.log.len(), 10);
    }

    #[test]
    fn sequence_numbers_never_repeat() {
        let mut model = Model::default();
        let first = model.next_key(100);
        let second = model.next_key(100);
        assert!(second > first);
    }
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;

    #[test]
    fn a_log_survives_a_json_round_trip() {
        let mut log = Log::default();
        for (seq, text) in ["first", "second"].into_iter().enumerate() {
            let mut message = Message::new(
                MessageKey {
                    time_ms: 100 + seq as u64,
                    seq: seq as u64,
                },
                "bob",
                MessageKind::Privmsg,
            );
            message.text = text.to_string();
            message.msgid = Some(alloc::format!("m{seq}"));
            log.insert(message);
        }

        // JSON cannot spell a structured object key, so the log travels as a list
        let json = serde_json::to_string(&log).expect("a log serialises");
        assert!(json.contains('['), "the messages are a list, not an object");

        let restored: Log = serde_json::from_str(&json).expect("and comes back");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get("m1").map(|m| m.text.as_str()), Some("second"));
        let texts: Vec<&str> = restored.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"], "and keeps its order");
    }

    #[test]
    fn a_whole_model_serialises() {
        let map = Casemapping::Ascii;
        let mut model = Model::default();
        let mut message = Message::new(
            MessageKey { time_ms: 1, seq: 0 },
            "bob",
            MessageKind::Privmsg,
        );
        message.text = "hello".to_string();
        model
            .channel_mut(map.fold("#obby"), "#obby")
            .log
            .insert(message);
        serde_json::to_string(&model).expect("the model a host reads must serialise");
    }
}
