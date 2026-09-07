//! Replay a recorded session and snapshot what the engine did with it.
//!
//! A transcript is a sequence of server lines. The engine is fed each one, and the snapshot records
//! everything it wrote back plus the model it ended up with. A protocol change that alters either
//! shows up as a snapshot diff to read, which is the point: the interesting regressions here are
//! changes in what we send and what we believe, not in any single function's return value.
//!
//! Transcripts live in `tests/transcripts`. A line beginning with `<` is from the server; anything
//! else is a comment. `\x01` is written as an escape so a transcript stays a readable text file.
//!
//! Review a changed snapshot with `cargo insta test` and accept it with `cargo insta accept`. A
//! changed snapshot is a behaviour change: read the diff, never accept blindly.

use std::fmt::Write as _;

use obby_client::{Client, Config, Event};

/// Replay one transcript, returning the snapshot body.
fn replay(source: &str) -> String {
    let mut client = Client::new(Config::new("me"));
    client.handle_connected();

    let mut sent = Vec::new();
    let mut events = Vec::new();
    drain(&mut client, &mut sent, &mut events);

    for line in source.lines() {
        let Some(line) = line.strip_prefix("< ") else {
            continue;
        };
        let line = line.replace("\\x01", "\u{1}");
        client.handle_bytes(format!("{line}\r\n").as_bytes());
        drain(&mut client, &mut sent, &mut events);
    }

    render(&client, &sent, &events)
}

fn drain(client: &mut Client, sent: &mut Vec<String>, events: &mut Vec<Event>) {
    while let Some(bytes) = client.poll_transmit() {
        sent.push(String::from_utf8_lossy(&bytes).trim_end().to_string());
    }
    while let Some(event) = client.poll_event() {
        events.push(event);
    }
}

fn render(client: &Client, sent: &[String], events: &[Event]) -> String {
    let mut out = String::new();

    out.push_str("== sent ==\n");
    for line in sent {
        let _ = writeln!(out, "{line}");
    }

    let _ = write!(
        out,
        "\n== connection ==\nphase: {:?}\nnick: {}\ncasemapping: {:?}\n",
        client.phase(),
        client.nick(),
        client.casemapping()
    );

    render_channels(client, &mut out);

    for (key, _) in client.model().channels() {
        if let Some(room) = client.voice_room(key) {
            let _ = writeln!(
                out,
                "\n== voice room {} ==\nkind {:?}, {} participants",
                room.channel,
                room.kind,
                room.participants.len()
            );
        }
    }

    for (key, whois) in client.model().whois_records() {
        let _ = writeln!(out, "\n== whois {key} ==\n{whois:?}");
    }

    for (key, bot) in client.bots().iter() {
        let commands: Vec<&str> = bot.commands.iter().map(|c| c.name.as_str()).collect();
        let _ = writeln!(
            out,
            "\n== bot {key} ==\n{} (id {:?}, from config {})  commands: {}",
            bot.nick,
            bot.id,
            bot.from_config,
            commands.join(" ")
        );
    }

    let allowed: Vec<&str> = client.allowed_commands().iter().collect();
    if !allowed.is_empty() {
        let _ = writeln!(
            out,
            "\n== commands the server allows ==\n{}",
            allowed.join(" ")
        );
    }

    out.push_str("\n== events ==\n");
    for event in events {
        let _ = writeln!(out, "{event:?}");
    }

    out
}

/// Every channel: its modes, who is in it, and what was said.
fn render_channels(client: &Client, out: &mut String) {
    out.push_str("\n== channels ==\n");
    for (key, channel) in client.model().channels() {
        let _ = writeln!(
            out,
            "{} (key {})  unread {} mentions {}",
            channel.name, key, channel.unread, channel.mentions
        );
        if let Some(marker) = &channel.read_marker {
            let _ = writeln!(out, "  read to: {marker}");
        }
        if let Some(topic) = &channel.topic {
            let _ = writeln!(out, "  topic: {topic}");
        }
        if !channel.named_modes.is_empty() {
            let named: Vec<String> = channel
                .named_modes
                .iter()
                .map(|(name, param)| match param {
                    Some(param) => format!("{name}={param}"),
                    None => name.clone(),
                })
                .collect();
            let _ = writeln!(out, "  named modes: {}", named.join(" "));
        }
        if !channel.modes.is_empty() {
            let modes: Vec<String> = channel
                .modes
                .iter()
                .map(|(mode, arg)| match arg {
                    Some(arg) => format!("{mode}={arg}"),
                    None => mode.to_string(),
                })
                .collect();
            let _ = writeln!(out, "  modes: {}", modes.join(" "));
        }
        for (member_key, member) in &channel.members {
            let person = client.model().person(member_key);
            let nick = person.map_or("?", |p| p.nick.as_str());
            let account = person
                .and_then(|p| p.account.as_ref())
                .map_or(String::new(), |a| format!(" account={a}"));
            let away = person
                .and_then(|p| p.away.as_ref())
                .map_or(String::new(), |a| format!(" away={a}"));
            let mut metadata = String::new();
            for (key, value) in person.iter().flat_map(|p| &p.metadata) {
                let _ = write!(metadata, " {key}={value}");
            }
            let _ = writeln!(
                out,
                "  member {}{nick} (key {member_key}){account}{away}{metadata}",
                member.prefixes
            );
        }
        for message in &channel.log {
            let _ = writeln!(
                out,
                "  [{}] {:?} <{}> {}{}",
                message.key.time_ms,
                message.kind,
                message.sender,
                message.text,
                if message.historical {
                    "  (history)"
                } else {
                    ""
                }
            );
            if message.redacted {
                let _ = writeln!(out, "      redacted");
            }
            if let Some(preview) = &message.link_preview {
                let _ = writeln!(out, "      preview: {}", preview.title);
            }
            for (emoji, who) in &message.reactions {
                let _ = writeln!(out, "      {emoji} {}", who.join(" "));
            }
        }
    }
}

#[test]
fn registration() {
    insta::assert_snapshot!(replay(include_str!("transcripts/registration.irc")));
}

#[test]
fn channel() {
    insta::assert_snapshot!(replay(include_str!("transcripts/channel.irc")));
}

#[test]
fn conversation() {
    insta::assert_snapshot!(replay(include_str!("transcripts/conversation.irc")));
}

#[test]
fn obby_extensions() {
    insta::assert_snapshot!(replay(include_str!("transcripts/obby.irc")));
}

#[test]
fn history() {
    insta::assert_snapshot!(replay(include_str!("transcripts/history.irc")));
}
