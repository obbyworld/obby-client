//! Turning protocol lines into model changes.
//!
//! Kept apart from the connection state machine in `client.rs`, which is only concerned with getting
//! registered and staying connected. Everything here assumes registration already happened.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use obby_proto::{CaseFolded, Casemapping, Isupport, Message as Line, parse_channel_modes};

use crate::model::{Message, MessageKind, Model};

/// What changed, for a host that wants to react without diffing the whole model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Change {
    /// A message landed in a channel or a conversation.
    Message {
        /// The channel or nick it belongs to, as the server spells it.
        target: String,
        /// Where it sits in that target's log.
        key: crate::model::MessageKey,
    },
    /// We joined a channel.
    Joined {
        /// The channel.
        channel: String,
    },
    /// We left a channel, whether by choice or by being removed.
    Parted {
        /// The channel.
        channel: String,
    },
    /// A channel's member list changed.
    MembersChanged {
        /// The channel.
        channel: String,
    },
    /// A channel's topic changed.
    TopicChanged {
        /// The channel.
        channel: String,
    },
    /// Someone changed nick, possibly us.
    NickChanged {
        /// What they were called.
        from: String,
        /// What they are called now.
        to: String,
    },
    /// A channel's modes changed.
    ModesChanged {
        /// The channel.
        channel: String,
    },
    /// The server confirmed how far we have read, so the unread counts moved.
    ReadMarker {
        /// The channel or person.
        target: String,
    },
    /// A message gained or lost a reaction.
    Reacted {
        /// Where the message is.
        target: String,
        /// The message reacted to.
        msgid: String,
    },
    /// A message was deleted.
    Redacted {
        /// Where the message was.
        target: String,
        /// The message deleted.
        msgid: String,
    },
    /// A metadata key changed on a person, a channel, or us.
    Metadata {
        /// Whose metadata changed.
        target: String,
        /// The key that changed.
        key: String,
    },
}

/// The token we tag our own WHOX requests with, so a reply to somebody else's is ignored.
pub(crate) const WHOX_TOKEN: &str = "332";

/// The WHOX fields we ask for, in the order the reply returns them.
///
/// `t` is the token, then channel, user, host, nick, flags, account and realname.
pub(crate) const WHOX_FIELDS: &str = "%tcuhnfar";

/// True when this character may appear in a nick.
///
/// Letters and digits, plus the punctuation RFC 1459 allows. It decides where a nick ends, so that
/// `bob` does not match inside `bobby`.
fn is_nick_char(c: char) -> bool {
    c.is_alphanumeric() || "[]\\`_^{|}-".contains(c)
}

/// True when this text addresses `nick`, under the server's casemapping.
///
/// The fold is the whole point. A server folding the bracket alphabet treats `[nick]` and `{NICK}`
/// as one person, and an ASCII lowercase misses that, which is why highlights are unreliable in the
/// reference client.
fn mentions(casemapping: Casemapping, text: &str, nick: &str) -> bool {
    if nick.is_empty() {
        return false;
    }
    let folded_nick = casemapping.fold(nick);
    let needle = folded_nick.as_str();
    let folded: String = text.chars().map(|c| casemapping.fold_char(c)).collect();

    let mut from = 0;
    while let Some(offset) = folded.get(from..).and_then(|rest| rest.find(needle)) {
        let start = from + offset;
        let end = start + needle.len();
        let before = folded.get(..start).and_then(|s| s.chars().next_back());
        let after = folded.get(end..).and_then(|s| s.chars().next());
        if !before.is_some_and(is_nick_char) && !after.is_some_and(is_nick_char) {
            return true;
        }
        from = end;
    }
    false
}

/// Everything needed to fold one line into the model.
pub(crate) struct Context<'a> {
    pub(crate) model: &'a mut Model,
    pub(crate) isupport: &'a Isupport,
    /// The latest timestamp seen, which stamps a line the server did not stamp itself.
    pub(crate) latest_ms: &'a mut u64,
    /// True while replaying a history batch rather than reading live traffic.
    pub(crate) historical: bool,
}

impl Context<'_> {
    fn casemapping(&self) -> Casemapping {
        self.isupport.casemapping()
    }

    fn fold(&self, name: &str) -> CaseFolded {
        self.isupport.fold(name)
    }

    /// When a line happened.
    ///
    /// `server-time` is authoritative. Without it we reuse the latest instant already seen rather
    /// than reading a clock, which keeps ordering stable and keeps this whole layer free of time.
    fn stamp(&mut self, line: &Line) -> u64 {
        let stamped = line
            .tag("time")
            .and_then(obby_proto::parse_server_time)
            .unwrap_or(*self.latest_ms);
        *self.latest_ms = (*self.latest_ms).max(stamped);
        stamped
    }

    fn is_me(&self, nick: &str) -> bool {
        self.casemapping().eq(&self.model.me.nick, nick)
    }

    fn sender(line: &Line) -> String {
        line.source
            .as_ref()
            .map_or_else(String::new, |source| source.name.clone())
    }
}

/// Fold one line into the model, returning what changed.
pub(crate) fn apply(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let command = line.command.to_ascii_uppercase();
    match command.as_str() {
        "PRIVMSG" | "NOTICE" | "TAGMSG" => message(ctx, line, &command),
        "JOIN" => join(ctx, line),
        "PART" => part(ctx, line),
        "QUIT" => quit(ctx, line),
        "KICK" => kick(ctx, line),
        "NICK" => nick(ctx, line),
        "TOPIC" => topic(ctx, line),
        "MODE" => mode(ctx, line),
        "332" => topic_reply(ctx, line),
        "MARKREAD" => mark_read(ctx, line),
        "REDACT" => redact(ctx, line),
        "AWAY" => away(ctx, line),
        "ACCOUNT" => account(ctx, line),
        // the server push and the reply to our own GET carry the same fields, one parameter apart
        "METADATA" => metadata(ctx, line, 0),
        // 760 is WHOIS surfacing metadata inline, with the same shape as a 761 reply
        "760" | "761" | "766" => metadata(ctx, line, 1),
        "353" => names(ctx, line),
        "352" => who_reply(ctx, line),
        "354" => whox_reply(ctx, line),
        "PROP" => prop(ctx, line),
        "961" => prop_list(ctx, line),
        _ => Vec::new(),
    }
}

fn message(ctx: &mut Context<'_>, line: &Line, command: &str) -> Vec<Change> {
    let Some(target) = line.param(0) else {
        return Vec::new();
    };
    if command == "TAGMSG"
        && let Some(changes) = reaction(ctx, line)
    {
        return changes;
    }
    #[cfg(feature = "obby")]
    if command == "TAGMSG"
        && let Some(changes) = link_preview(ctx, line)
    {
        return changes;
    }

    let sender = Context::sender(line);
    let time_ms = ctx.stamp(line);
    let key = ctx.model.next_key(time_ms);
    let body = if command == "TAGMSG" {
        String::new()
    } else {
        line.param(1).unwrap_or_default().to_string()
    };

    let (kind, text) = match command {
        "TAGMSG" => (MessageKind::Tagmsg, body),
        "NOTICE" => (MessageKind::Notice, body),
        _ => match obby_proto::parse_ctcp(&body) {
            Some(ctcp) => (
                MessageKind::Ctcp {
                    command: ctcp.command.to_ascii_uppercase(),
                },
                ctcp.params,
            ),
            None => (MessageKind::Privmsg, body),
        },
    };

    let mut message = Message::new(key, sender.clone(), kind);
    message.text = text;
    message.msgid = line.tag("msgid").map(ToString::to_string);
    message.account = line.tag("account").map(ToString::to_string);
    message.own = ctx.is_me(&sender);
    message.historical = ctx.historical;
    // the server sends `+reply` from one module and `+draft/reply` from another, so both spellings
    // have to be accepted or threading silently breaks depending on which module answered
    message.reply_to = line
        .tag("+reply")
        .or_else(|| line.tag("+draft/reply"))
        .map(ToString::to_string);

    // a message to `@#channel` is addressed to the operators of that channel and still belongs in
    // it, so the status prefix comes off before we decide what the target is
    let addressed = target.trim_start_matches(|c| ctx.isupport.is_statusmsg(c));
    // a message addressed to us belongs in the conversation with whoever sent it, not in one named
    // after ourselves
    let (name, is_channel) = if ctx.isupport.is_channel(addressed) {
        (addressed.to_string(), true)
    } else if message.own {
        (target.to_string(), false)
    } else {
        (sender, false)
    };

    // history and our own words are never unread, and nothing counts as addressing us twice
    let counts = !message.own && !message.historical;
    let addressed = counts && mentions(ctx.casemapping(), &message.text, &ctx.model.me.nick);

    let folded = ctx.fold(&name);
    // a channel only exists because we joined it. Without this a server can name arbitrarily many
    // channels we are not in and grow the model without limit, which `join` already refuses
    if is_channel && ctx.model.channel(&folded).is_none() {
        return Vec::new();
    }
    let accepted = if is_channel {
        let channel = ctx.model.channel_mut(folded, &name);
        let accepted = channel.log.insert(message);
        if accepted && counts {
            channel.unread = channel.unread.saturating_add(1);
            if addressed {
                channel.mentions = channel.mentions.saturating_add(1);
            }
        }
        accepted
    } else {
        let query = ctx.model.query_mut(folded, &name);
        let accepted = query.log.insert(message);
        if accepted && counts {
            // a private message is addressed to us by existing at all
            query.unread = query.unread.saturating_add(1);
        }
        accepted
    };

    if accepted {
        alloc::vec![Change::Message { target: name, key }]
    } else {
        Vec::new()
    }
}

fn join(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(channel) = line.param(0) else {
        return Vec::new();
    };
    let who = Context::sender(line);
    let name = channel.to_string();
    let folded = ctx.fold(&name);

    if ctx.is_me(&who) {
        ctx.model.channel_mut(folded, &name);
        return alloc::vec![Change::Joined { channel: name }];
    }

    let account = line.param(1).filter(|a| *a != "*").map(ToString::to_string);
    // only our own JOIN brings a channel into being. A server naming channels we are not in would
    // otherwise grow the model without limit, and every one of them would be a lie
    let key = ctx.isupport.fold(&who);
    if ctx.model.channel(&folded).is_none() {
        return Vec::new();
    }
    let person = ctx.model.person_mut(key.clone(), &who);
    person.nick = who;
    // extended-join carries the account on the JOIN itself, sparing us a WHO for it
    if account.is_some() {
        person.account = account;
    }
    if let Some(channel) = ctx.model.existing_channel_mut(&folded) {
        channel.members.entry(key).or_default();
    }
    alloc::vec![Change::MembersChanged { channel: name }]
}

fn part(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(channel) = line.param(0) else {
        return Vec::new();
    };
    let who = Context::sender(line);
    let name = channel.to_string();
    let folded = ctx.fold(&name);

    if ctx.is_me(&who) {
        ctx.model.remove_channel(&folded);
        return alloc::vec![Change::Parted { channel: name }];
    }
    if let Some(channel) = ctx.model.existing_channel_mut(&folded) {
        channel.members.remove(&ctx.isupport.fold(&who));
    }
    ctx.model.forget_strangers();
    alloc::vec![Change::MembersChanged { channel: name }]
}

fn quit(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let who = Context::sender(line);
    let folded = ctx.fold(&who);
    let left = ctx.model.remove_everywhere(&folded);
    ctx.model.forget_strangers();
    left.into_iter()
        .filter_map(|key| {
            ctx.model
                .channel(&key)
                .map(|channel| Change::MembersChanged {
                    channel: channel.name.clone(),
                })
        })
        .collect()
}

fn kick(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let (Some(channel), Some(target)) = (line.param(0), line.param(1)) else {
        return Vec::new();
    };
    let name = channel.to_string();
    let folded = ctx.fold(&name);

    if ctx.is_me(target) {
        ctx.model.remove_channel(&folded);
        return alloc::vec![Change::Parted { channel: name }];
    }
    let removed = ctx.isupport.fold(target);
    if let Some(channel) = ctx.model.existing_channel_mut(&folded) {
        channel.members.remove(&removed);
    }
    ctx.model.forget_strangers();
    alloc::vec![Change::MembersChanged { channel: name }]
}

fn nick(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(to) = line.param(0) else {
        return Vec::new();
    };
    let from = Context::sender(line);
    ctx.model.rename(ctx.isupport.casemapping(), &from, to);
    alloc::vec![Change::NickChanged {
        from,
        to: to.to_string()
    }]
}

fn topic(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(channel) = line.param(0) else {
        return Vec::new();
    };
    let name = channel.to_string();
    let folded = ctx.fold(&name);
    let who = Context::sender(line);
    let text = line.param(1).unwrap_or_default().to_string();
    let Some(entry) = ctx.model.existing_channel_mut(&folded) else {
        return Vec::new();
    };
    entry.topic = (!text.is_empty()).then_some(text);
    entry.topic_by = Some(who);
    alloc::vec![Change::TopicChanged { channel: name }]
}

fn topic_reply(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(channel) = line.param(1) else {
        return Vec::new();
    };
    let name = channel.to_string();
    let folded = ctx.fold(&name);
    let text = line.param(2).unwrap_or_default().to_string();
    let Some(entry) = ctx.model.existing_channel_mut(&folded) else {
        return Vec::new();
    };
    entry.topic = (!text.is_empty()).then_some(text);
    alloc::vec![Change::TopicChanged { channel: name }]
}

fn names(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    // `353 <nick> <symbol> <channel> :<names>`
    let Some(channel) = line.param(2) else {
        return Vec::new();
    };
    let name = channel.to_string();
    let folded = ctx.fold(&name);
    let entries: Vec<String> = line
        .trailing()
        .unwrap_or_default()
        .split_whitespace()
        .map(ToString::to_string)
        .collect();

    for entry in entries {
        let (prefixes, nick) = ctx.isupport.prefix().split(&entry);
        // userhost-in-names turns the entry into a full hostmask
        let nick = nick.split('!').next().unwrap_or(nick);
        if nick.is_empty() {
            continue;
        }
        let (prefixes, nick, key) = (
            prefixes.to_string(),
            nick.to_string(),
            ctx.isupport.fold(nick),
        );
        if ctx.model.channel(&folded).is_none() {
            return Vec::new();
        }
        ctx.model
            .person_mut(key.clone(), &nick)
            .nick
            .clone_from(&nick);
        if let Some(channel) = ctx.model.existing_channel_mut(&folded) {
            channel.members.entry(key).or_default().prefixes = prefixes;
        }
    }
    alloc::vec![Change::MembersChanged { channel: name }]
}

/// Apply a reaction carried on a TAGMSG, if that is what this is.
///
/// Returns `None` when the line is an ordinary TAGMSG, so the caller files it as a message.
fn reaction(ctx: &mut Context<'_>, line: &Line) -> Option<Vec<Change>> {
    let (tag, adding) = match (line.tag("+draft/react"), line.tag("+draft/unreact")) {
        (Some(emoji), _) => (emoji, true),
        (None, Some(emoji)) => (emoji, false),
        (None, None) => return None,
    };
    let emoji = tag.to_string();
    // the server is inconsistent about which spelling of the reply tag it sends, so both are read
    let msgid = line
        .tag("+reply")
        .or_else(|| line.tag("+draft/reply"))?
        .to_string();
    let sender = Context::sender(line);
    // a reactor is a person, and two spellings of one nick are one person, so this folds like every
    // other identity rather than comparing the raw string
    let who = ctx.fold(&sender).into_string();
    let target = line.param(0)?;
    let name = target.to_string();
    let folded = ctx.fold(&name);

    let log = match ctx.model.existing_channel_mut(&folded) {
        Some(channel) => &mut channel.log,
        None => &mut ctx.model.existing_query_mut(&folded)?.log,
    };
    let message = log.get_mut(&msgid)?;
    let reactors = message.reactions.entry(emoji.clone()).or_default();
    if adding {
        if !reactors.contains(&who) {
            reactors.push(who);
        }
    } else {
        reactors.retain(|reactor| *reactor != who);
        if reactors.is_empty() {
            message.reactions.remove(&emoji);
        }
    }
    Some(alloc::vec![Change::Reacted {
        target: name,
        msgid
    }])
}

/// Attach a server-built link preview to the message it describes.
///
/// It arrives as a bare TAGMSG rather than as part of the message, because the server has to fetch
/// the page before it knows what to say, long after the message itself went out.
#[cfg(feature = "obby")]
fn link_preview(ctx: &mut Context<'_>, line: &Line) -> Option<Vec<Change>> {
    let (msgid, preview) = crate::extensions::LinkPreview::parse(line)?;
    // past this point the line is a preview whatever happens next. A message we no longer hold, or
    // never held, leaves nothing to attach it to, and filing it as a message of its own would put an
    // empty row in the conversation
    let mut changes = Vec::new();
    if let Some(name) = line.param(0) {
        let name = name.to_string();
        let folded = ctx.fold(&name);
        let log = match ctx.model.existing_channel_mut(&folded) {
            Some(channel) => Some(&mut channel.log),
            None => ctx
                .model
                .existing_query_mut(&folded)
                .map(|query| &mut query.log),
        };
        if let Some(log) = log
            && let Some(message) = log.get_mut(&msgid)
        {
            message.link_preview = Some(preview);
            let key = message.key;
            changes.push(Change::Message { target: name, key });
        }
    }
    Some(changes)
}

/// Mark a message deleted, keeping what it said.
///
/// Throwing the content away leaves no way to show who deleted what, and a client that has already
/// shown the message to someone gains nothing by forgetting it.
fn redact(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let (Some(target), Some(msgid)) = (line.param(0), line.param(1)) else {
        return Vec::new();
    };
    let name = target.to_string();
    let folded = ctx.fold(&name);
    let msgid = msgid.to_string();

    let Some(log) = (match ctx.model.existing_channel_mut(&folded) {
        Some(channel) => Some(&mut channel.log),
        None => ctx.model.existing_query_mut(&folded).map(|q| &mut q.log),
    }) else {
        return Vec::new();
    };
    let Some(message) = log.get_mut(&msgid) else {
        return Vec::new();
    };
    message.redacted = true;
    alloc::vec![Change::Redacted {
        target: name,
        msgid
    }]
}

/// Apply one metadata key to whoever it belongs to.
///
/// `offset` is where the target sits: a server push names it first, while a numeric reply puts our
/// own nick there and shifts everything along by one.
///
/// A key with no value is a deletion. `766` says the key is not set, which is the same thing.
fn metadata(ctx: &mut Context<'_>, line: &Line, offset: usize) -> Vec<Change> {
    let (Some(target), Some(key)) = (line.param(offset), line.param(offset + 1)) else {
        return Vec::new();
    };
    // a target of `*` means us, which is how a server answers before it knows our nick
    let target = if target == "*" {
        ctx.model.me.nick.clone()
    } else {
        target.to_string()
    };
    let key = key.to_string();
    // `761` carries a visibility between the key and the value; `766` has neither
    let value = line
        .param(offset + 3)
        .filter(|_| !line.is("766"))
        .map(ToString::to_string);
    let folded = ctx.fold(&target);

    let apply = |store: &mut BTreeMap<String, String>| match &value {
        Some(value) => {
            store.insert(key.clone(), value.clone());
        }
        None => {
            store.remove(&key);
        }
    };

    if ctx.isupport.is_channel(&target) {
        let Some(channel) = ctx.model.existing_channel_mut(&folded) else {
            return Vec::new();
        };
        apply(&mut channel.metadata);
        return alloc::vec![Change::Metadata { target, key }];
    }

    apply(&mut ctx.model.person_mut(folded, &target).metadata);
    if ctx.is_me(&target) {
        apply(&mut ctx.model.me.metadata);
    }
    alloc::vec![Change::Metadata { target, key }]
}

/// Note that someone went away or came back.
fn away(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let reason = line.param(0).map(ToString::to_string);
    person_changed(ctx, line, |person| person.away.clone_from(&reason))
}

/// Note that someone logged in or out of an account.
fn account(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let name = line.param(0).filter(|a| *a != "*").map(ToString::to_string);
    person_changed(ctx, line, |person| person.account.clone_from(&name))
}

/// Apply a change to whoever sent the line, and report every channel it shows in.
fn person_changed(
    ctx: &mut Context<'_>,
    line: &Line,
    change: impl FnOnce(&mut crate::model::Person),
) -> Vec<Change> {
    let who = Context::sender(line);
    let key = ctx.fold(&who);
    change(ctx.model.person_mut(key.clone(), &who));
    if ctx.is_me(&who) {
        let person = ctx.model.person(&key).cloned().unwrap_or_default();
        ctx.model.me.away = person.away;
        ctx.model.me.account = person.account;
    }
    ctx.model
        .channels()
        .filter(|(_, channel)| channel.members.contains_key(&key))
        .map(|(_, channel)| Change::MembersChanged {
            channel: channel.name.clone(),
        })
        .collect()
}

/// Record how far the server says we have read, and clear what that covers.
///
/// The marker is server-authoritative: we set it by sending `MARKREAD` and only believe it when the
/// server says so, which is what keeps two clients on one account agreeing.
fn mark_read(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let (Some(target), Some(timestamp)) = (line.param(0), line.param(1)) else {
        return Vec::new();
    };
    let marker = timestamp
        .strip_prefix("timestamp=")
        .unwrap_or(timestamp)
        .to_string();
    let folded = ctx.fold(target);
    let name = target.to_string();

    if let Some(channel) = ctx.model.existing_channel_mut(&folded) {
        channel.read_marker = Some(marker);
        channel.unread = 0;
        channel.mentions = 0;
        return alloc::vec![Change::ReadMarker { target: name }];
    }
    if let Some(query) = ctx.model.existing_query_mut(&folded) {
        query.read_marker = Some(marker);
        query.unread = 0;
        return alloc::vec![Change::ReadMarker { target: name }];
    }
    Vec::new()
}

/// Apply a plain `WHO` reply.
///
/// `352 <client> <channel> <user> <host> <server> <nick> <flags> :<hops> <realname>`. Nothing here
/// carries an account, which is why the WHOX form exists and why we ask for it when we can.
fn who_reply(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let (Some(channel), Some(user), Some(host), Some(nick), Some(flags)) = (
        line.param(1),
        line.param(2),
        line.param(3),
        line.param(5),
        line.param(6),
    ) else {
        return Vec::new();
    };
    // the trailing parameter is a hop count and the realname, separated by a space
    let realname = line
        .trailing()
        .and_then(|trailing| trailing.split_once(' '))
        .map(|(_, realname)| realname.to_string());
    apply_who(
        ctx,
        WhoRow {
            channel,
            nick,
            user,
            host,
            flags,
            account: Account::Unknown,
            realname,
        },
    )
}

/// Apply a WHOX reply, which is the same information plus the account.
///
/// The field order follows what we ask for, so this only reads a reply carrying our own token.
fn whox_reply(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    if line.param(1) != Some(WHOX_TOKEN) {
        return Vec::new();
    }
    let (Some(channel), Some(user), Some(host), Some(nick), Some(flags), Some(account)) = (
        line.param(2),
        line.param(3),
        line.param(4),
        line.param(5),
        line.param(6),
        line.param(7),
    ) else {
        return Vec::new();
    };
    let account = if account == "0" || account == "*" {
        Account::LoggedOut
    } else {
        Account::As(account.to_string())
    };
    apply_who(
        ctx,
        WhoRow {
            channel,
            nick,
            user,
            host,
            flags,
            account,
            realname: line.param(8).map(ToString::to_string),
        },
    )
}

/// What one WHO or WHOX row says about somebody.
struct WhoRow<'a> {
    channel: &'a str,
    nick: &'a str,
    user: &'a str,
    host: &'a str,
    flags: &'a str,
    account: Account,
    realname: Option<String>,
}

/// What a reply says about somebody's account, which is not the same question as what it is.
///
/// A plain WHO carries no account field at all, and treating that as "logged out" would sign
/// everybody out every time we refreshed a member list.
enum Account {
    /// The reply did not say.
    Unknown,
    /// The reply said they are logged in as nobody.
    LoggedOut,
    /// The reply named an account.
    As(String),
}

/// Fold one WHO or WHOX row into the person and, when it names a channel, their membership.
fn apply_who(ctx: &mut Context<'_>, row: WhoRow<'_>) -> Vec<Change> {
    let key = ctx.fold(row.nick);
    let prefixes: String = row
        .flags
        .chars()
        .filter(|c| ctx.isupport.prefix().rank(*c).is_some())
        .collect();
    let away = row.flags.starts_with('G');

    let person = ctx.model.person_mut(key.clone(), row.nick);
    person.nick = row.nick.to_string();
    person.username = Some(row.user.to_string());
    person.host = Some(row.host.to_string());
    person.operator = row.flags.contains('*');
    person.bot = row.flags.contains('B');
    if let Some(realname) = row.realname {
        person.realname = Some(realname);
    }
    // WHO says only whether someone is away, never why, so an existing message is kept rather than
    // replaced with nothing
    if away {
        person.away.get_or_insert_with(String::new);
    } else {
        person.away = None;
    }
    match row.account {
        Account::Unknown => {}
        Account::LoggedOut => person.account = None,
        Account::As(account) => person.account = Some(account),
    }

    if !ctx.isupport.is_channel(row.channel) {
        return Vec::new();
    }
    let name = row.channel.to_string();
    let folded = ctx.fold(&name);
    let Some(entry) = ctx.model.existing_channel_mut(&folded) else {
        return Vec::new();
    };
    entry.members.entry(key).or_default().prefixes = prefixes;
    alloc::vec![Change::MembersChanged { channel: name }]
}

/// Apply a named mode change.
///
/// `PROP <target> (+name[=param] | -name)+`. The server relays every legacy `MODE` as an equivalent
/// `PROP` to anyone holding the capability, so both arrive and both are applied. They agree, and
/// applying the same change twice is what makes that harmless.
fn prop(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(target) = line.param(0) else {
        return Vec::new();
    };
    let name = target.to_string();
    let folded = ctx.fold(&name);
    let changes: Vec<String> = line.params.iter().skip(1).cloned().collect();
    if !apply_named(ctx, &folded, changes.iter().map(String::as_str)) {
        return Vec::new();
    }
    alloc::vec![Change::ModesChanged { channel: name }]
}

/// Apply one line of a `PROP` listing, which reports state rather than a change.
///
/// `961 <client> <target> <name>[=<param>]...`, with no leading sign, so every entry is set.
fn prop_list(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(target) = line.param(1) else {
        return Vec::new();
    };
    let name = target.to_string();
    let folded = ctx.fold(&name);
    let entries: Vec<String> = line
        .params
        .iter()
        .skip(2)
        .flat_map(|param| param.split_whitespace())
        .map(|entry| alloc::format!("+{entry}"))
        .collect();
    if !apply_named(ctx, &folded, entries.iter().map(String::as_str)) {
        return Vec::new();
    }
    alloc::vec![Change::ModesChanged { channel: name }]
}

/// Fold `+name[=param]` and `-name` entries into a channel's named modes.
///
/// Returns false when we are not in the channel, so a server naming one we never joined does not
/// bring it into being.
fn apply_named<'a>(
    ctx: &mut Context<'_>,
    folded: &CaseFolded,
    entries: impl Iterator<Item = &'a str>,
) -> bool {
    let Some(channel) = ctx.model.existing_channel_mut(folded) else {
        return false;
    };
    for entry in entries {
        if let Some(rest) = entry.strip_prefix('+') {
            let (name, param) = rest.split_once('=').map_or((rest, None), |(name, param)| {
                (name, Some(param.to_string()))
            });
            channel.named_modes.insert(name.to_string(), param);
        } else if let Some(name) = entry.strip_prefix('-') {
            // a removal may still carry the parameter it is removing, which is not part of the name
            let name = name.split('=').next().unwrap_or(name);
            channel.named_modes.remove(name);
        }
    }
    true
}

fn mode(ctx: &mut Context<'_>, line: &Line) -> Vec<Change> {
    let Some(target) = line.param(0) else {
        return Vec::new();
    };
    if !ctx.isupport.is_channel(target) {
        for change in obby_proto::parse_user_modes(line.param(1).unwrap_or_default()) {
            if change.set {
                if !ctx.model.me.modes.contains(change.mode) {
                    ctx.model.me.modes.push(change.mode);
                }
            } else {
                ctx.model.me.modes.retain(|m| m != change.mode);
            }
        }
        return Vec::new();
    }

    let name = target.to_string();
    let folded = ctx.fold(&name);
    let args: Vec<String> = line.params.iter().skip(1).cloned().collect();
    let changes = parse_channel_modes(ctx.isupport, &args);
    let order = ctx.isupport.prefix().clone();

    let mut touched_members = false;
    for change in changes {
        if change.membership {
            let Some(nick) = change.arg else { continue };
            let Some(prefix) = order.char_for_mode(change.mode) else {
                continue;
            };
            let key = ctx.isupport.fold(&nick);
            if let Some(member) = ctx
                .model
                .existing_channel_mut(&folded)
                .and_then(|channel| channel.members.get_mut(&key))
            {
                if change.set {
                    member.grant(prefix, &order);
                } else {
                    member.revoke(prefix);
                }
                touched_members = true;
            }
            continue;
        }
        let Some(channel) = ctx.model.existing_channel_mut(&folded) else {
            continue;
        };
        if change.set {
            channel.modes.insert(change.mode, change.arg);
        } else {
            channel.modes.remove(&change.mode);
        }
    }

    if touched_members {
        alloc::vec![
            Change::ModesChanged {
                channel: name.clone()
            },
            Change::MembersChanged { channel: name },
        ]
    } else {
        alloc::vec![Change::ModesChanged { channel: name }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MessageKind;

    struct Harness {
        model: Model,
        isupport: Isupport,
        latest_ms: u64,
        historical: bool,
    }

    impl Harness {
        fn new() -> Self {
            let mut isupport = Isupport::default();
            isupport.apply("CHANTYPES=#^$");
            isupport.apply("PREFIX=(qaohv)~&@%+");
            isupport.apply("CHANMODES=beI,k,l,imnpst");
            let mut model = Model::default();
            model.me.nick = "me".to_string();
            Self {
                model,
                isupport,
                latest_ms: 1000,
                historical: false,
            }
        }

        /// Put us in a channel, which is the only thing that brings one into being.
        fn joined(mut self, channel: &str) -> Self {
            self.feed(&alloc::format!(":me!u@h JOIN {channel}"));
            self
        }

        fn feed(&mut self, raw: &str) -> Vec<Change> {
            let line = Line::parse(raw).expect("the test line should parse");
            let mut ctx = Context {
                model: &mut self.model,
                isupport: &self.isupport,
                latest_ms: &mut self.latest_ms,
                historical: self.historical,
            };
            apply(&mut ctx, &line)
        }

        fn channel(&self, name: &str) -> &crate::model::Channel {
            self.model
                .channel(&self.isupport.fold(name))
                .expect("the channel should exist")
        }

        fn person(&self, nick: &str) -> &crate::model::Person {
            self.model
                .person(&self.isupport.fold(nick))
                .expect("we should know this person")
        }

        fn query(&self, nick: &str) -> &crate::model::Query {
            self.model
                .query(&self.isupport.fold(nick))
                .expect("the conversation should exist")
        }
    }

    #[test]
    fn a_message_to_a_channel_we_never_joined_invents_nothing() {
        let mut h = Harness::new();
        assert!(h.feed(":bob!u@h PRIVMSG #ghost :hello").is_empty());
        assert_eq!(
            h.model.channels().count(),
            0,
            "a server naming channels we are not in would otherwise grow the model without limit"
        );
    }

    #[test]
    fn a_channel_message_lands_in_that_channel() {
        let mut h = Harness::new().joined("#obby");
        let changes = h.feed(":bob!u@h PRIVMSG #obby :hello");
        assert!(
            matches!(changes.first(), Some(Change::Message { target, .. }) if target == "#obby")
        );
        let message = h.channel("#obby").log.last().expect("a message");
        assert_eq!(message.text, "hello");
        assert_eq!(message.sender, "bob");
        assert!(!message.own);
    }

    #[test]
    fn a_private_message_is_filed_under_the_sender_not_under_us() {
        let mut h = Harness::new();
        h.feed(":bob!u@h PRIVMSG me :psst");
        assert_eq!(h.query("bob").log.len(), 1);
        assert!(
            h.model.query(&h.isupport.fold("me")).is_none(),
            "a conversation named after ourselves would be nobody"
        );
    }

    #[test]
    fn our_own_private_message_is_filed_under_who_we_sent_it_to() {
        let mut h = Harness::new();
        h.feed(":me!u@h PRIVMSG bob :hi there");
        let message = h.query("bob").log.last().expect("a message");
        assert!(message.own);
        assert_eq!(message.text, "hi there");
    }

    #[test]
    fn joining_creates_the_channel_and_others_joining_add_members() {
        let mut h = Harness::new();
        assert_eq!(
            h.feed(":me!u@h JOIN #obby"),
            [Change::Joined {
                channel: "#obby".to_string()
            }]
        );
        h.feed(":bob!u@h JOIN #obby");
        assert_eq!(h.channel("#obby").members.len(), 1);
        assert_eq!(h.person("bob").nick.as_str(), "bob");
    }

    #[test]
    fn extended_join_carries_the_account_without_a_who() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby alice :Bob");
        assert_eq!(h.person("bob").account.as_deref(), Some("alice"));
    }

    #[test]
    fn an_unauthenticated_join_records_no_account() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby * :Bob");
        assert_eq!(
            h.person("bob").account,
            None,
            "a star means logged in as nobody"
        );
    }

    #[test]
    fn leaving_forgets_the_channel_but_someone_else_leaving_does_not() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby");
        h.feed(":bob!u@h PART #obby :bye");
        assert!(h.channel("#obby").members.is_empty());

        h.feed(":me!u@h PART #obby :bye");
        assert!(h.model.channel(&h.isupport.fold("#obby")).is_none());
    }

    #[test]
    fn being_kicked_forgets_the_channel() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby");
        h.feed(":op!u@h KICK #obby bob :rude");
        assert!(h.channel("#obby").members.is_empty());

        h.feed(":op!u@h KICK #obby me :rude");
        assert!(h.model.channel(&h.isupport.fold("#obby")).is_none());
    }

    #[test]
    fn a_quit_reports_every_channel_they_were_in() {
        let mut h = Harness::new().joined("#a").joined("#b");
        for name in ["#a", "#b"] {
            h.feed(&alloc::format!(":bob!u@h JOIN {name}"));
        }
        let changes = h.feed(":bob!u@h QUIT :gone");
        assert_eq!(changes.len(), 2);
        assert!(h.channel("#a").members.is_empty());
        assert!(h.channel("#b").members.is_empty());
    }

    #[test]
    fn a_rename_follows_the_servers_casemapping() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":[bob]!u@h JOIN #obby");
        h.feed(":[bob]!u@h NICK {BOB}");
        let channel = h.channel("#obby");
        assert_eq!(
            channel.members.len(),
            1,
            "rfc1459 folds braces onto brackets, so this is one person"
        );
        assert!(channel.members.contains_key(&h.isupport.fold("{bob}")));
        assert_eq!(h.person("{bob}").nick, "{BOB}");
    }

    #[test]
    fn names_splits_prefixes_and_hostmasks() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 353 me = #obby :~&@alice +bob carol!u@host");
        let channel = h.channel("#obby");
        assert_eq!(channel.members.len(), 3);
        assert_eq!(
            channel
                .members
                .get(&h.isupport.fold("alice"))
                .map(|m| m.prefixes.as_str()),
            Some("~&@")
        );
        assert_eq!(
            channel
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("+")
        );
        assert!(
            channel.members.contains_key(&h.isupport.fold("carol")),
            "userhost-in-names must not become part of the nick"
        );
    }

    #[test]
    fn a_membership_mode_moves_a_prefix_and_a_channel_mode_does_not() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby");
        h.feed(":op!u@h MODE #obby +o bob");
        assert_eq!(
            h.channel("#obby")
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("@")
        );

        h.feed(":op!u@h MODE #obby +b nuisance!*@*");
        assert_eq!(
            h.channel("#obby")
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("@"),
            "a ban must not consume the nick argument of a membership mode"
        );
        assert!(h.channel("#obby").modes.contains_key(&'b'));

        h.feed(":op!u@h MODE #obby -o bob");
        assert_eq!(
            h.channel("#obby")
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("")
        );
    }

    #[test]
    fn our_own_user_modes_accumulate() {
        let mut h = Harness::new();
        h.feed(":s MODE me +iw");
        assert_eq!(h.model.me.modes, "iw");
        h.feed(":s MODE me -i");
        assert_eq!(h.model.me.modes, "w");
    }

    #[test]
    fn the_topic_arrives_from_a_command_or_a_numeric() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 332 me #obby :from the numeric");
        assert_eq!(
            h.channel("#obby").topic.as_deref(),
            Some("from the numeric")
        );
        h.feed(":bob!u@h TOPIC #obby :from the command");
        assert_eq!(
            h.channel("#obby").topic.as_deref(),
            Some("from the command")
        );
        assert_eq!(h.channel("#obby").topic_by.as_deref(), Some("bob"));
    }

    #[test]
    fn clearing_the_topic_leaves_none_rather_than_an_empty_string() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h TOPIC #obby :something");
        h.feed(":bob!u@h TOPIC #obby :");
        assert_eq!(h.channel("#obby").topic, None);
    }

    #[test]
    fn server_time_decides_when_a_message_happened() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@time=2026-09-06T10:00:00.000Z :bob!u@h PRIVMSG #obby :stamped");
        let message = h.channel("#obby").log.last().expect("a message");
        assert_eq!(message.key.time_ms, 1_788_688_800_000);
    }

    #[test]
    fn an_unstamped_line_reuses_the_latest_instant_seen() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@time=2026-09-06T10:00:00.000Z :bob!u@h PRIVMSG #obby :stamped");
        h.feed(":bob!u@h PRIVMSG #obby :unstamped");
        let messages: Vec<u64> = h
            .channel("#obby")
            .log
            .iter()
            .map(|m| m.key.time_ms)
            .collect();
        assert_eq!(
            messages,
            [1_788_688_800_000, 1_788_688_800_000],
            "an unstamped line must not sort back before the stamped ones"
        );
    }

    #[test]
    fn a_ctcp_keeps_its_command_and_loses_its_wrapper() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h PRIVMSG #obby :\u{1}ACTION waves\u{1}");
        let message = h.channel("#obby").log.last().expect("a message");
        assert_eq!(
            message.kind,
            MessageKind::Ctcp {
                command: "ACTION".to_string()
            }
        );
        assert_eq!(message.text, "waves");
    }

    #[test]
    fn both_spellings_of_the_reply_tag_are_accepted() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@+reply=abc :bob!u@h PRIVMSG #obby :one");
        h.feed("@+draft/reply=def :bob!u@h PRIVMSG #obby :two");
        let replies: Vec<Option<&str>> = h
            .channel("#obby")
            .log
            .iter()
            .map(|m| m.reply_to.as_deref())
            .collect();
        assert_eq!(
            replies,
            [Some("abc"), Some("def")],
            "the server sends one spelling from filehost and the other from pushbot"
        );
    }

    #[test]
    fn a_repeated_msgid_is_only_stored_once() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@msgid=x1 :bob!u@h PRIVMSG #obby :hello");
        h.feed("@msgid=x1 :bob!u@h PRIVMSG #obby :hello");
        assert_eq!(h.channel("#obby").log.len(), 1);
    }

    #[test]
    fn a_mention_needs_a_whole_nick_not_a_substring() {
        let map = Casemapping::Rfc1459;
        assert!(mentions(map, "hey me, look", "me"));
        assert!(mentions(map, "me: look", "me"));
        assert!(mentions(map, "me", "me"));
        assert!(
            !mentions(map, "spameda", "me"),
            "a nick inside a word is not a mention"
        );
        assert!(!mentions(map, "me-too", "me"), "a dash is a nick character");
        assert!(!mentions(map, "", "me"));
        assert!(!mentions(map, "anything", ""));
    }

    #[test]
    fn a_mention_folds_the_way_the_server_does() {
        assert!(
            mentions(Casemapping::Rfc1459, "hey {NICK} there", "[nick]"),
            "rfc1459 folds braces onto brackets, so this addresses us"
        );
        assert!(
            !mentions(Casemapping::Ascii, "hey {NICK} there", "[nick]"),
            "an ascii server treats them as different people"
        );
        assert!(mentions(Casemapping::Ascii, "hey NICK there", "nick"));
    }

    #[test]
    fn a_channel_message_counts_as_unread_and_a_mention_only_when_it_names_us() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h PRIVMSG #obby :morning all");
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (1, 0)
        );

        h.feed(":bob!u@h PRIVMSG #obby :me: got a second?");
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (2, 1)
        );
    }

    #[test]
    fn our_own_words_and_replayed_history_are_never_unread() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":me!u@h PRIVMSG #obby :me talking about me");
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (0, 0)
        );

        h.historical = true;
        h.feed(":bob!u@h PRIVMSG #obby :me, an old message");
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (0, 0),
            "scrolling back must not light up the unread badge"
        );
    }

    #[test]
    fn every_private_message_is_addressed_to_us_by_existing() {
        let mut h = Harness::new();
        h.feed(":bob!u@h PRIVMSG me :no nick needed");
        assert_eq!(h.query("bob").unread, 1);
    }

    #[test]
    fn the_server_confirming_a_read_marker_clears_what_it_covers() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h PRIVMSG #obby :me: hello");
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (1, 1)
        );

        let changes = h.feed(":s MARKREAD #obby timestamp=2026-09-06T10:00:00.000Z");
        assert_eq!(
            changes,
            [Change::ReadMarker {
                target: "#obby".to_string()
            }]
        );
        assert_eq!(
            (h.channel("#obby").unread, h.channel("#obby").mentions),
            (0, 0)
        );
        assert_eq!(
            h.channel("#obby").read_marker.as_deref(),
            Some("2026-09-06T10:00:00.000Z")
        );
    }

    #[test]
    fn a_reaction_attaches_to_the_message_rather_than_becoming_one() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@msgid=m1 :bob!u@h PRIVMSG #obby :something");
        let changes = h.feed("@+draft/react=👍;+draft/reply=m1 :carol!u@h TAGMSG #obby");
        assert_eq!(
            changes,
            [Change::Reacted {
                target: "#obby".to_string(),
                msgid: "m1".to_string()
            }]
        );
        assert_eq!(
            h.channel("#obby").log.len(),
            1,
            "a reaction is not a message in the log"
        );
        let message = h.channel("#obby").log.get("m1").expect("the message");
        assert_eq!(
            message.reactions.get("👍").map(Vec::as_slice),
            Some(&["carol".to_string()][..])
        );
    }

    #[test]
    fn reacting_twice_counts_once_and_taking_it_back_removes_it() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@msgid=m1 :bob!u@h PRIVMSG #obby :something");
        h.feed("@+draft/react=👍;+reply=m1 :carol!u@h TAGMSG #obby");
        h.feed("@+draft/react=👍;+reply=m1 :carol!u@h TAGMSG #obby");
        assert_eq!(
            h.channel("#obby")
                .log
                .get("m1")
                .expect("message")
                .reactions
                .get("👍")
                .map(Vec::len),
            Some(1)
        );

        h.feed("@+draft/unreact=👍;+reply=m1 :carol!u@h TAGMSG #obby");
        assert!(
            h.channel("#obby")
                .log
                .get("m1")
                .expect("message")
                .reactions
                .is_empty(),
            "the last reactor leaving takes the emoji with them"
        );
    }

    #[test]
    fn a_redaction_marks_the_message_and_keeps_what_it_said() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@msgid=m1 :bob!u@h PRIVMSG #obby :regrettable");
        let changes = h.feed(":op!u@h REDACT #obby m1 :spam");
        assert_eq!(
            changes,
            [Change::Redacted {
                target: "#obby".to_string(),
                msgid: "m1".to_string()
            }]
        );
        let message = h.channel("#obby").log.get("m1").expect("the message stays");
        assert!(message.redacted);
        assert_eq!(
            message.text, "regrettable",
            "discarding the content leaves no way to show who deleted what"
        );
    }

    #[test]
    fn going_away_shows_on_every_member_list_they_are_in() {
        let mut h = Harness::new().joined("#a").joined("#b");
        h.feed(":bob!u@h JOIN #a");
        h.feed(":bob!u@h JOIN #b");
        h.feed(":bob!u@h AWAY :back later");
        for name in ["#a", "#b"] {
            assert!(
                h.channel(name)
                    .members
                    .contains_key(&h.isupport.fold("bob"))
            );
        }
        assert_eq!(
            h.person("bob").away.as_deref(),
            Some("back later"),
            "away is recorded once, not once per channel"
        );

        h.feed(":bob!u@h AWAY");
        assert_eq!(
            h.model
                .person(&h.isupport.fold("bob"))
                .and_then(|p| p.away.as_deref()),
            None
        );
    }

    #[test]
    fn logging_into_an_account_updates_everywhere_they_are() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby");
        h.feed(":bob!u@h ACCOUNT alice");
        assert_eq!(
            h.model
                .person(&h.isupport.fold("bob"))
                .and_then(|p| p.account.as_deref()),
            Some("alice")
        );

        h.feed(":bob!u@h ACCOUNT *");
        assert_eq!(
            h.model
                .person(&h.isupport.fold("bob"))
                .and_then(|p| p.account.as_deref()),
            None,
            "a star means logged out"
        );
    }

    #[test]
    fn our_own_away_and_account_land_on_us() {
        let mut h = Harness::new();
        h.feed(":me!u@h AWAY :lunch");
        assert_eq!(h.model.me.away.as_deref(), Some("lunch"));
        h.feed(":me!u@h ACCOUNT myaccount");
        assert_eq!(h.model.me.account.as_deref(), Some("myaccount"));
    }

    #[test]
    fn a_tagmsg_carrying_nothing_we_model_is_still_a_message() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@+example.org/thing=1 :bob!u@h TAGMSG #obby");
        assert_eq!(h.channel("#obby").log.len(), 1);
    }

    #[test]
    fn metadata_about_a_person_is_held_once_for_them() {
        let mut h = Harness::new().joined("#a").joined("#b");
        h.feed(":bob!u@h JOIN #a");
        h.feed(":bob!u@h JOIN #b");
        let changes = h.feed(":s METADATA bob display-name * :Bobby Tables");
        assert_eq!(
            changes,
            [Change::Metadata {
                target: "bob".to_string(),
                key: "display-name".to_string()
            }]
        );
        assert_eq!(
            h.person("bob")
                .metadata
                .get("display-name")
                .map(String::as_str),
            Some("Bobby Tables"),
            "one person, one record, however many channels we share"
        );
    }

    #[test]
    fn a_key_with_no_value_clears_it() {
        let mut h = Harness::new();
        h.feed(":s METADATA bob avatar * :https://example.org/a.png");
        assert!(h.person("bob").metadata.contains_key("avatar"));
        h.feed(":s METADATA bob avatar *");
        assert!(
            !h.person("bob").metadata.contains_key("avatar"),
            "a push with no value is how the server deletes a key"
        );
    }

    #[test]
    fn the_not_set_reply_clears_a_key_too() {
        let mut h = Harness::new();
        h.feed(":s 761 me bob color * :#ff0000");
        assert_eq!(
            h.person("bob").metadata.get("color").map(String::as_str),
            Some("#ff0000")
        );
        h.feed(":s 766 me bob color :no matching key");
        assert!(!h.person("bob").metadata.contains_key("color"));
    }

    #[test]
    fn a_numeric_reply_reads_past_our_own_nick() {
        let mut h = Harness::new();
        h.feed(":s 761 me bob display-name * :Bobby");
        assert_eq!(
            h.person("bob")
                .metadata
                .get("display-name")
                .map(String::as_str),
            Some("Bobby"),
            "the numeric puts our nick first, so the target is one parameter along"
        );
    }

    #[test]
    fn channel_metadata_lands_on_the_channel_not_on_a_person() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s METADATA #obby avatar * :https://example.org/c.png");
        assert_eq!(
            h.channel("#obby")
                .metadata
                .get("avatar")
                .map(String::as_str),
            Some("https://example.org/c.png")
        );
        assert!(h.model.person(&h.isupport.fold("#obby")).is_none());
    }

    #[test]
    fn a_star_target_means_us() {
        let mut h = Harness::new();
        h.feed(":s METADATA * display-name * :Me Myself");
        assert_eq!(
            h.model.me.metadata.get("display-name").map(String::as_str),
            Some("Me Myself")
        );
    }

    #[cfg(feature = "obby")]
    #[test]
    fn a_link_preview_attaches_to_its_message_instead_of_becoming_one() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@msgid=m1 :bob!u@h PRIVMSG #obby :look at https://example.org");
        h.feed(
            "@+reply=m1;obsidianirc/link-preview-title=Example;obsidianirc/link-preview-snippet=A\\spage :s TAGMSG #obby",
        );
        assert_eq!(
            h.channel("#obby").log.len(),
            1,
            "the preview arrives long after the message, and must not become a second one"
        );
        let preview = h
            .channel("#obby")
            .log
            .get("m1")
            .expect("the message")
            .link_preview
            .as_ref()
            .expect("a preview");
        assert_eq!(preview.title, "Example");
        assert_eq!(preview.snippet.as_deref(), Some("A page"));
    }

    #[cfg(feature = "obby")]
    #[test]
    fn a_preview_for_a_message_we_never_saw_is_dropped() {
        let mut h = Harness::new().joined("#obby");
        h.feed("@+reply=gone;obsidianirc/link-preview-title=Example :s TAGMSG #obby");
        assert_eq!(
            h.channel("#obby").log.len(),
            0,
            "there is nothing to attach it to, and it is not a message of its own"
        );
    }

    #[test]
    fn a_who_reply_fills_in_who_someone_actually_is() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 352 me #obby ident host.example irc.example bob H@ :0 Bob Smith");
        let person = h.person("bob");
        assert_eq!(person.username.as_deref(), Some("ident"));
        assert_eq!(person.host.as_deref(), Some("host.example"));
        assert_eq!(person.realname.as_deref(), Some("Bob Smith"));
        assert_eq!(person.away, None, "H means here");
        assert_eq!(
            h.channel("#obby")
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("@")
        );
    }

    #[test]
    fn a_plain_who_never_signs_anybody_out() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h JOIN #obby alice_acct :Bob");
        assert_eq!(h.person("bob").account.as_deref(), Some("alice_acct"));

        h.feed(":s 352 me #obby ident host.example irc.example bob H :0 Bob");
        assert_eq!(
            h.person("bob").account.as_deref(),
            Some("alice_acct"),
            "a plain WHO carries no account field, which is not the same as an empty one"
        );
    }

    #[test]
    fn a_whox_reply_carries_the_account() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 354 me 332 #obby ident host.example bob H+ alice_acct :Bob Smith");
        let person = h.person("bob");
        assert_eq!(person.account.as_deref(), Some("alice_acct"));
        assert_eq!(person.realname.as_deref(), Some("Bob Smith"));
        assert_eq!(
            h.channel("#obby")
                .members
                .get(&h.isupport.fold("bob"))
                .map(|m| m.prefixes.as_str()),
            Some("+")
        );
    }

    #[test]
    fn a_whox_reply_can_sign_somebody_out() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 354 me 332 #obby ident host bob H acct :Bob");
        assert!(h.person("bob").account.is_some());
        h.feed(":s 354 me 332 #obby ident host bob H 0 :Bob");
        assert_eq!(
            h.person("bob").account,
            None,
            "a zero in the account field is how WHOX spells logged out"
        );
    }

    #[test]
    fn a_whox_reply_to_somebody_elses_request_is_ignored() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 354 me 999 #obby ident host bob H acct :Bob");
        assert!(
            h.model.person(&h.isupport.fold("bob")).is_none(),
            "the token is what tells our reply apart from another client's"
        );
    }

    #[test]
    fn who_reports_away_and_operator_and_bot_flags() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 352 me #obby ident host irc bob G*B@ :0 Bob");
        let person = h.person("bob");
        assert!(person.away.is_some(), "G means gone");
        assert!(person.operator);
        assert!(person.bot);
    }

    #[test]
    fn coming_back_from_away_clears_it_but_going_away_keeps_a_known_reason() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h AWAY :at lunch");
        h.feed(":s 352 me #obby ident host irc bob G :0 Bob");
        assert_eq!(
            h.person("bob").away.as_deref(),
            Some("at lunch"),
            "WHO says only that they are away, so a reason we already have survives"
        );
        h.feed(":s 352 me #obby ident host irc bob H :0 Bob");
        assert_eq!(h.person("bob").away, None);
    }

    #[test]
    fn a_named_mode_is_recorded_by_its_name_not_its_letter() {
        let mut h = Harness::new().joined("#obby");
        let changes = h.feed(":s PROP #obby +obsidianirc/censor +obsidianirc/history=30d");
        assert_eq!(
            changes,
            [Change::ModesChanged {
                channel: "#obby".to_string()
            }]
        );
        let modes = &h.channel("#obby").named_modes;
        assert_eq!(modes.get("obsidianirc/censor"), Some(&None));
        assert_eq!(
            modes.get("obsidianirc/history"),
            Some(&Some("30d".to_string())),
            "a letter means nothing without the server saying what it does; the name is stable"
        );
    }

    #[test]
    fn removing_a_named_mode_ignores_the_parameter_it_carries() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s PROP #obby +obsidianirc/history=30d");
        h.feed(":s PROP #obby -obsidianirc/history=30d");
        assert!(
            !h.channel("#obby")
                .named_modes
                .contains_key("obsidianirc/history")
        );
    }

    #[test]
    fn a_prop_listing_reports_state_rather_than_a_change() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":s 961 me #obby obsidianirc/censor obsidianirc/history=7d");
        let modes = &h.channel("#obby").named_modes;
        assert_eq!(
            modes.len(),
            2,
            "a listing has no signs, so every entry is set"
        );
        assert_eq!(
            modes.get("obsidianirc/history"),
            Some(&Some("7d".to_string()))
        );
    }

    #[test]
    fn a_prop_for_a_channel_we_never_joined_invents_nothing() {
        let mut h = Harness::new();
        assert!(h.feed(":s PROP #ghost +obsidianirc/censor").is_empty());
        assert_eq!(h.model.channels().count(), 0);
    }

    #[test]
    fn the_same_change_arriving_as_mode_and_as_prop_is_harmless() {
        let mut h = Harness::new().joined("#obby");
        // the server relays every legacy MODE as an equivalent PROP to capability holders
        h.feed(":op!u@h MODE #obby +t");
        h.feed(":s PROP #obby +topiclock");
        assert!(h.channel("#obby").modes.contains_key(&'t'));
        assert!(h.channel("#obby").named_modes.contains_key("topiclock"));
    }

    #[test]
    fn a_notice_is_not_an_ordinary_message() {
        let mut h = Harness::new().joined("#obby");
        h.feed(":bob!u@h NOTICE #obby :careful");
        assert_eq!(
            h.channel("#obby").log.last().map(|m| m.kind.clone()),
            Some(MessageKind::Notice)
        );
    }
}
