//! The Obby vendored extensions.
//!
//! Implemented against the wire the running server and client speak, rather than against the
//! published <https://github.com/obbyworld/extensions>. The two disagree in
//! about twenty places: the repository names a batch type and an attribution tag for channel-bots
//! that do not exist on the wire, and describes a `manage-bots` permission that exists in neither
//! the server nor the client.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use obby_proto::Message as Line;

/// A preview of a link someone posted, built by the server and attached to the message.
///
/// The server fetches the page; a client never does. There is no capability to negotiate, and the
/// server refuses these tags from any sender but itself, so a peer cannot forge one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct LinkPreview {
    /// The page title. Always present when a preview exists at all.
    pub title: String,
    /// A description of the page, when the page offered one.
    pub snippet: Option<String>,
    /// An image, already re-hosted by the server when it has a filehost configured.
    pub image: Option<String>,
}

impl LinkPreview {
    /// Read a preview off a TAGMSG, with the message it describes.
    ///
    /// The tags carry no capability and arrive on a bare TAGMSG whose `+reply` names the message
    /// being previewed. Both spellings of the reply tag are accepted, because the server sends one
    /// from its filehost module and the other from its bot module.
    pub fn parse(line: &Line) -> Option<(String, Self)> {
        let title = line.tag("obsidianirc/link-preview-title")?.to_string();
        let msgid = line
            .tag("+reply")
            .or_else(|| line.tag("+draft/reply"))?
            .to_string();
        Some((
            msgid,
            Self {
                title,
                snippet: line
                    .tag("obsidianirc/link-preview-snippet")
                    .map(ToString::to_string),
                image: line
                    .tag("obsidianirc/link-preview-meta")
                    .map(ToString::to_string),
            },
        ))
    }
}

/// The set of commands the server says we may currently use.
///
/// The server pushes this on connect and again whenever it changes, such as after an `OPER`. A
/// batch carries additions and removals together, so both are applied at once.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "AllowedCommands"))]
pub struct Commands {
    available: alloc::collections::BTreeSet<String>,
}

impl Commands {
    /// Know about no commands yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one `CMDSLIST` line's worth of `+name` and `-name` tokens.
    ///
    /// A long list is split across several lines inside one batch, so this is called per line and
    /// the effects accumulate.
    pub fn apply(&mut self, line: &Line) {
        for token in line
            .params
            .iter()
            .flat_map(|param| param.split_whitespace())
        {
            if let Some(name) = token.strip_prefix('+') {
                self.available.insert(name.to_ascii_uppercase());
            } else if let Some(name) = token.strip_prefix('-') {
                self.available.remove(&name.to_ascii_uppercase());
            }
        }
    }

    /// True when the server says we may use this command.
    pub fn contains(&self, name: &str) -> bool {
        self.available.contains(&name.to_ascii_uppercase())
    }

    /// Every command we may use, in name order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.available.iter().map(String::as_str)
    }

    /// How many commands we may use.
    pub fn len(&self) -> usize {
        self.available.len()
    }

    /// True when the server has told us nothing yet.
    pub fn is_empty(&self) -> bool {
        self.available.is_empty()
    }
}

/// An invitation link to the network or to one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Invitation {
    /// The identifier used to delete it.
    pub share_id: String,
    /// The channel it joins, or nothing when it invites to the network.
    pub channel: Option<String>,
    /// The link itself.
    pub url: String,
    /// When it was made, as the server spells it.
    pub created: Option<String>,
    /// How many people have used it.
    pub redeemed: u32,
    /// What it is for.
    pub description: Option<String>,
}

impl Invitation {
    /// Read the `INVITELINK` reply that follows a create.
    ///
    /// The shape is `INVITELINK <share-id> <channel|*> :<url>`.
    pub fn parse_created(line: &Line) -> Option<Self> {
        let share_id = line.param(0)?;
        if share_id.eq_ignore_ascii_case("ENTRY") {
            return None;
        }
        Some(Self {
            share_id: share_id.to_string(),
            channel: channel_or_network(line.param(1)?),
            url: line.param(2)?.to_string(),
            created: None,
            redeemed: 0,
            description: None,
        })
    }

    /// Read one line of an `INVITELINK LIST` reply.
    ///
    /// The shape is `INVITELINK ENTRY <share-id> <channel|*> <created> <redeemed> <url>
    /// [:<description>]`.
    pub fn parse_entry(line: &Line) -> Option<Self> {
        if !line.param(0)?.eq_ignore_ascii_case("ENTRY") {
            return None;
        }
        Some(Self {
            share_id: line.param(1)?.to_string(),
            channel: channel_or_network(line.param(2)?),
            created: Some(line.param(3)?.to_string()),
            // a count we cannot read is not a reason to drop the whole invitation
            redeemed: line.param(4).and_then(|n| n.parse().ok()).unwrap_or(0),
            url: line.param(5)?.to_string(),
            description: line.param(6).map(ToString::to_string),
        })
    }
}

/// `*` in place of a channel means the invitation is to the network rather than to one channel.
fn channel_or_network(value: &str) -> Option<String> {
    (value != "*").then(|| value.to_string())
}

/// What the server knows about a bot in a channel.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Bot {
    /// The bot's nick.
    pub nick: String,
    /// The identifier the server gave it.
    pub id: Option<String>,
    /// True when an operator configured this bot, rather than it registering itself.
    ///
    /// Only a configured bot may claim a privileged command name. A bot that registered itself has
    /// those names stripped, so it cannot shadow `oper` or `identify`.
    pub from_config: bool,
    /// The commands it offers.
    pub commands: Vec<BotCommand>,
}

/// One command a bot offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct BotCommand {
    /// What to type, without its leading slash.
    pub name: String,
    /// What it does.
    pub description: Option<String>,
}

/// Command names a self-registered bot must never be allowed to claim.
///
/// Letting one shadow these lets it collect a password or impersonate a server service. Only a bot
/// an operator configured may register them.
pub const PRIVILEGED_COMMANDS: &[&str] = &[
    "oper", "identify", "nickserv", "chanserv", "ns", "cs", "register", "pass", "auth", "login",
];

/// The bots we know about, keyed by their folded nick.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Bots {
    known: BTreeMap<String, Bot>,
}

impl Bots {
    /// Know about no bots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a bot the server told us about.
    pub fn insert(&mut self, key: String, bot: Bot) {
        self.known.insert(key, bot);
    }

    /// Forget a bot.
    pub fn remove(&mut self, key: &str) -> Option<Bot> {
        self.known.remove(key)
    }

    /// A bot we know about.
    pub fn get(&self, key: &str) -> Option<&Bot> {
        self.known.get(key)
    }

    /// Every bot, in folded nick order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Bot)> {
        self.known.iter()
    }

    /// How many bots we know about.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    /// True when we know of none.
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Record the commands a bot offers, dropping any name it is not entitled to.
    ///
    /// A bot we have never been told about is refused outright. Accepting a command list from an
    /// arbitrary nick would let anyone put entries in the command menu, and the reference client
    /// has exactly that gap on its parallel workflow protocol.
    pub fn set_commands(&mut self, key: &str, commands: Vec<BotCommand>) -> bool {
        let Some(bot) = self.known.get_mut(key) else {
            return false;
        };
        bot.commands = if bot.from_config {
            commands
        } else {
            commands
                .into_iter()
                .filter(|command| !is_privileged(&command.name))
                .collect()
        };
        true
    }
}

/// True when this command name may only be claimed by a bot an operator configured.
pub fn is_privileged(name: &str) -> bool {
    PRIVILEGED_COMMANDS
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(raw: &str) -> Line {
        Line::parse(raw).expect("the test line should parse")
    }

    #[test]
    fn a_preview_names_the_message_it_describes() {
        let (msgid, preview) = LinkPreview::parse(&line(
            "@+reply=m1;obsidianirc/link-preview-title=Example;obsidianirc/link-preview-snippet=A\\spage;obsidianirc/link-preview-meta=https://h/i.png :s TAGMSG #obby",
        ))
        .expect("a preview");
        assert_eq!(msgid, "m1");
        assert_eq!(preview.title, "Example");
        assert_eq!(preview.snippet.as_deref(), Some("A page"));
        assert_eq!(preview.image.as_deref(), Some("https://h/i.png"));
    }

    #[test]
    fn a_preview_needs_only_a_title() {
        let (_, preview) = LinkPreview::parse(&line(
            "@+draft/reply=m1;obsidianirc/link-preview-title=Bare :s TAGMSG #obby",
        ))
        .expect("a preview");
        assert_eq!(preview.snippet, None);
        assert_eq!(preview.image, None);
    }

    #[test]
    fn a_preview_with_nothing_to_attach_to_is_not_one() {
        assert!(
            LinkPreview::parse(&line(
                "@obsidianirc/link-preview-title=Orphan :s TAGMSG #obby"
            ))
            .is_none(),
            "without a reply tag there is no message to hang it on"
        );
        assert!(LinkPreview::parse(&line("@+reply=m1 :s TAGMSG #obby")).is_none());
    }

    #[test]
    fn the_command_list_adds_and_removes_in_one_pass() {
        let mut commands = Commands::new();
        commands.apply(&line(":s CMDSLIST +JOIN +PART +OPER"));
        assert_eq!(commands.len(), 3);
        assert!(
            commands.contains("join"),
            "command names are case-insensitive"
        );

        commands.apply(&line(":s CMDSLIST -OPER +TOPIC"));
        assert!(!commands.contains("OPER"));
        assert!(commands.contains("TOPIC"));
        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn a_command_list_split_across_lines_accumulates() {
        let mut commands = Commands::new();
        commands.apply(&line(":s CMDSLIST +JOIN +PART"));
        commands.apply(&line(":s CMDSLIST +TOPIC"));
        assert_eq!(
            commands.len(),
            3,
            "a long list arrives as several lines in one batch"
        );
    }

    #[test]
    fn a_created_invitation_reads_back() {
        let invitation = Invitation::parse_created(&line(
            ":s INVITELINK abc123 #obby :https://obby.example/i/abc123",
        ))
        .expect("an invitation");
        assert_eq!(invitation.share_id, "abc123");
        assert_eq!(invitation.channel.as_deref(), Some("#obby"));
        assert_eq!(invitation.url, "https://obby.example/i/abc123");
    }

    #[test]
    fn a_star_means_the_invitation_is_to_the_network() {
        let invitation =
            Invitation::parse_created(&line(":s INVITELINK abc123 * :https://obby.example/i/abc"))
                .expect("an invitation");
        assert_eq!(invitation.channel, None);
    }

    #[test]
    fn a_list_entry_carries_its_history() {
        let invitation = Invitation::parse_entry(&line(
            ":s INVITELINK ENTRY abc123 #obby 2026-09-06T10:00:00Z 4 https://obby.example/i/abc :for the team",
        ))
        .expect("an entry");
        assert_eq!(invitation.share_id, "abc123");
        assert_eq!(invitation.created.as_deref(), Some("2026-09-06T10:00:00Z"));
        assert_eq!(invitation.redeemed, 4);
        assert_eq!(invitation.description.as_deref(), Some("for the team"));
    }

    #[test]
    fn an_unreadable_count_does_not_lose_the_invitation() {
        let invitation = Invitation::parse_entry(&line(
            ":s INVITELINK ENTRY abc123 * 2026-09-06T10:00:00Z lots https://obby.example/i/abc",
        ))
        .expect("an entry");
        assert_eq!(invitation.redeemed, 0);
        assert_eq!(invitation.url, "https://obby.example/i/abc");
    }

    #[test]
    fn a_created_reply_is_not_mistaken_for_a_list_entry() {
        assert!(
            Invitation::parse_created(&line(":s INVITELINK ENTRY a * 2026 0 https://u")).is_none()
        );
        assert!(Invitation::parse_entry(&line(":s INVITELINK abc * :https://u")).is_none());
    }

    #[test]
    fn a_self_registered_bot_cannot_claim_a_privileged_name() {
        let mut bots = Bots::new();
        bots.insert(
            "helper".to_string(),
            Bot {
                nick: "helper".to_string(),
                from_config: false,
                ..Bot::default()
            },
        );
        assert!(bots.set_commands(
            "helper",
            alloc::vec![
                BotCommand {
                    name: "weather".to_string(),
                    description: None
                },
                BotCommand {
                    name: "IdentIfy".to_string(),
                    description: None
                },
            ]
        ));
        let commands = &bots.get("helper").expect("the bot").commands;
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "weather");
    }

    #[test]
    fn a_configured_bot_may_claim_one() {
        let mut bots = Bots::new();
        bots.insert(
            "services".to_string(),
            Bot {
                nick: "services".to_string(),
                from_config: true,
                ..Bot::default()
            },
        );
        bots.set_commands(
            "services",
            alloc::vec![BotCommand {
                name: "identify".to_string(),
                description: None
            }],
        );
        assert_eq!(bots.get("services").expect("the bot").commands.len(), 1);
    }

    #[test]
    fn commands_from_a_nick_we_never_heard_of_are_refused() {
        let mut bots = Bots::new();
        assert!(
            !bots.set_commands("stranger", alloc::vec![]),
            "anyone could otherwise put entries in the command menu"
        );
        assert!(bots.is_empty());
    }

    #[test]
    fn privileged_names_are_matched_regardless_of_case() {
        assert!(is_privileged("OPER"));
        assert!(is_privileged("NickServ"));
        assert!(!is_privileged("weather"));
    }
}
