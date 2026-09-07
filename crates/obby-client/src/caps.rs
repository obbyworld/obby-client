//! Capability negotiation state.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// The capabilities this engine knows how to use, in the order we ask for them.
///
/// A capability we do not name here is never requested, because requesting one we cannot handle
/// changes what the server sends and breaks parsing.
pub const WANTED_CAPS: &[&str] = &[
    // parsing changes, so these come first
    "message-tags",
    "server-time",
    "batch",
    "labeled-response",
    "echo-message",
    "draft/multiline",
    "multi-prefix",
    "userhost-in-names",
    "extended-join",
    "account-notify",
    "account-tag",
    "away-notify",
    "chghost",
    "invite-notify",
    "setname",
    "cap-notify",
    "monitor",
    "extended-monitor",
    // messages. `reply`, `react` and `msgid` are client tags carried by message-tags rather than
    // capabilities, so requesting them would only earn a NAK.
    "draft/chathistory",
    "draft/event-playback",
    "draft/read-marker",
    "draft/typing",
    "draft/message-redaction",
    "draft/channel-rename",
    "channel-context",
    "draft/channel-context",
    "standard-replies",
    "draft/named-modes",
    "draft/metadata-2",
    "draft/extended-isupport",
    "draft/extended-isupport-0.2",
    "sasl",
    "draft/whoami",
    "draft/account-registration",
    "draft/account-2fa",
    // the -notify variant pushes network changes at us, so we never poll LISTNETWORKS
    "soju.im/bouncer-networks",
    "soju.im/bouncer-networks-notify",
    "znc.in/playback",
    #[cfg(feature = "obby")]
    "obsidianirc/cmdslist",
    #[cfg(feature = "obby")]
    "obby.world/channel-bots",
    #[cfg(feature = "obby")]
    "draft/bot-cmds",
    #[cfg(feature = "obby")]
    "draft/bot-tools",
    // without this the server falls back to unwrapped legacy WHOIS numerics
    #[cfg(feature = "obby")]
    "obby.world/whois",
    #[cfg(feature = "obby")]
    "obby.world/invitation",
    #[cfg(feature = "obby")]
    "draft/authtoken",
    // ObbyIRCd's own, despite the draft/ spelling: no such IRCv3 specification exists
    #[cfg(feature = "obby")]
    "draft/persistence",
    #[cfg(feature = "obby")]
    "unrealircd.org/json-log",
    #[cfg(feature = "voice")]
    "obsidianirc/voice",
];

/// One capability the server advertised, with the value it carried if any.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Capability {
    /// The capability name, without any `=value` suffix.
    pub name: String,
    /// The value, for capabilities like `sasl=PLAIN,EXTERNAL` that carry one.
    pub value: Option<String>,
}

/// What the server offers and what we hold.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "Capabilities"))]
pub struct Capabilities {
    available: BTreeMap<String, Option<String>>,
    acknowledged: BTreeMap<String, Option<String>>,
    /// Capabilities we asked for and are still waiting on. Registration cannot finish while this is
    /// non-empty, because `CAP END` before the last reply loses the capability.
    pending: Vec<String>,
}

impl Capabilities {
    /// Record one `CAP LS` or `CAP NEW` line's worth of advertisements.
    pub fn advertise(&mut self, list: &str) {
        for token in list.split_whitespace() {
            let (name, value) = split_value(token);
            self.available
                .insert(name.to_owned(), value.map(ToOwned::to_owned));
        }
    }

    /// Forget capabilities the server withdrew with `CAP DEL`.
    pub fn withdraw(&mut self, list: &str) {
        for token in list.split_whitespace() {
            let (name, _) = split_value(token);
            self.available.remove(name);
            self.acknowledged.remove(name);
        }
    }

    /// The subset of [`WANTED_CAPS`] the server offers and we have not asked for yet.
    pub fn to_request(&self) -> Vec<String> {
        WANTED_CAPS
            .iter()
            .filter(|name| self.available.contains_key(**name))
            .filter(|name| !self.acknowledged.contains_key(**name))
            .filter(|name| !self.pending.iter().any(|p| p == *name))
            .map(|name| (*name).to_owned())
            .collect()
    }

    /// Note that we sent a `CAP REQ` for these.
    pub fn requested(&mut self, names: &[String]) {
        self.pending.extend_from_slice(names);
    }

    /// Apply a `CAP ACK`, returning the names that were acknowledged.
    pub fn acknowledge(&mut self, list: &str) -> Vec<String> {
        let mut acked = Vec::new();
        for token in list.split_whitespace() {
            // an ack may arrive prefixed with `-`, meaning the server dropped a capability we held
            if let Some(name) = token.strip_prefix('-') {
                self.acknowledged.remove(name);
                self.pending.retain(|p| p != name);
                continue;
            }
            let (name, value) = split_value(token);
            // an ACK rarely repeats the value the LS carried, so fall back to what was advertised;
            // otherwise the SASL mechanism list disappears the moment the capability is granted
            let value = value
                .map(ToOwned::to_owned)
                .or_else(|| self.available.get(name).cloned().flatten());
            self.acknowledged.insert(name.to_owned(), value);
            self.pending.retain(|p| p != name);
            acked.push(name.to_owned());
        }
        acked
    }

    /// Apply a `CAP NAK`. Nothing is enabled, we only stop waiting.
    pub fn reject(&mut self, list: &str) {
        for token in list.split_whitespace() {
            let (name, _) = split_value(token);
            self.pending.retain(|p| p != name);
        }
    }

    /// True when we hold this capability.
    pub fn has(&self, name: &str) -> bool {
        self.acknowledged.contains_key(name)
    }

    /// The value the server gave a capability, such as the SASL mechanism list.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.acknowledged.get(name)?.as_deref()
    }

    /// True when the server advertised this capability, whether or not we hold it.
    pub fn offers(&self, name: &str) -> bool {
        self.available.contains_key(name)
    }

    /// True when every requested capability has been answered.
    pub fn settled(&self) -> bool {
        self.pending.is_empty()
    }

    /// Everything we hold, sorted by name.
    pub fn enabled(&self) -> impl Iterator<Item = Capability> + '_ {
        self.acknowledged.iter().map(|(name, value)| Capability {
            name: name.clone(),
            value: value.clone(),
        })
    }
}

fn split_value(token: &str) -> (&str, Option<&str>) {
    token
        .split_once('=')
        .map_or((token, None), |(name, value)| (name, Some(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn requests_only_what_is_offered_and_wanted() {
        let mut caps = Capabilities::default();
        caps.advertise("multi-prefix sasl=PLAIN,EXTERNAL something-we-never-want");
        let request = caps.to_request();
        assert!(request.contains(&"multi-prefix".to_string()));
        assert!(request.contains(&"sasl".to_string()));
        assert!(!request.iter().any(|c| c == "something-we-never-want"));
    }

    #[test]
    fn keeps_the_value_of_an_acknowledged_capability() {
        let mut caps = Capabilities::default();
        caps.advertise("sasl=PLAIN,EXTERNAL");
        caps.requested(&["sasl".to_string()]);
        caps.acknowledge("sasl=PLAIN,EXTERNAL");
        assert!(caps.has("sasl"));
        assert_eq!(caps.value("sasl"), Some("PLAIN,EXTERNAL"));
    }

    #[test]
    fn an_ack_without_a_value_keeps_the_advertised_one() {
        let mut caps = Capabilities::default();
        caps.advertise("sasl=PLAIN,EXTERNAL");
        caps.requested(&["sasl".to_string()]);
        caps.acknowledge("sasl");
        assert_eq!(caps.value("sasl"), Some("PLAIN,EXTERNAL"));
    }

    #[test]
    fn is_not_settled_until_every_request_is_answered() {
        let mut caps = Capabilities::default();
        caps.advertise("multi-prefix away-notify");
        let request = caps.to_request();
        caps.requested(&request);
        assert!(!caps.settled());
        caps.acknowledge("multi-prefix");
        assert!(!caps.settled());
        caps.reject("away-notify");
        assert!(caps.settled());
    }

    #[test]
    fn a_negated_ack_drops_the_capability() {
        let mut caps = Capabilities::default();
        caps.advertise("echo-message");
        caps.requested(&["echo-message".to_string()]);
        caps.acknowledge("echo-message");
        assert!(caps.has("echo-message"));
        caps.acknowledge("-echo-message");
        assert!(!caps.has("echo-message"));
    }

    #[test]
    fn cap_del_removes_an_offer_and_the_hold() {
        let mut caps = Capabilities::default();
        caps.advertise("away-notify");
        caps.requested(&["away-notify".to_string()]);
        caps.acknowledge("away-notify");
        caps.withdraw("away-notify");
        assert!(!caps.has("away-notify"));
        assert!(!caps.offers("away-notify"));
    }

    #[test]
    fn does_not_re_request_what_is_already_held() {
        let mut caps = Capabilities::default();
        caps.advertise("multi-prefix");
        caps.requested(&["multi-prefix".to_string()]);
        caps.acknowledge("multi-prefix");
        assert!(caps.to_request().is_empty());
    }
}
