//! A working client in one file: connect, join a channel, answer anyone who says hello.
//!
//! ```sh
//! cargo run --example echo-bot -- irc.libera.chat:6667 '#obby' mynick
//! ```
//!
//! Plain TCP, so it needs no TLS crate to read. For 6697 wrap the socket in whatever TLS stack you
//! already use; the engine never touches the socket either way.

use std::env;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use obby_client::{Change, Client, Config, Event, Now};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "irc.libera.chat:6667".into());
    let channel = args.next().unwrap_or_else(|| "#obby".into());
    let nick = args.next().unwrap_or_else(|| "obbybot".into());

    let mut socket = TcpStream::connect(&address)?;
    // the engine says when it next wants time, so a short read timeout is all the loop needs
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;

    let mut client = Client::new(Config::new(nick));
    client.handle_connected();

    let started = SystemTime::now();
    let mut buffer = [0u8; 8192];
    let mut out = std::io::stdout().lock();

    loop {
        while let Some(bytes) = client.poll_transmit() {
            socket.write_all(&bytes)?;
        }

        match socket.read(&mut buffer) {
            Ok(0) => {
                client.handle_disconnected();
                return Ok(());
            }
            Ok(read) => match buffer.get(..read) {
                Some(chunk) => client.handle_bytes(chunk),
                None => return Err("the socket read more than it was given".into()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }

        client.tick(now(started));

        while let Some(event) = client.poll_event() {
            match event {
                Event::Registered { nick } => {
                    writeln!(out, "registered as {nick}")?;
                    client.join(&channel, None);
                }
                Event::ModelChanged {
                    change: Change::MessageAdded { target, key },
                } => {
                    let folded = client.casemapping().fold(&target);
                    let Some(message) = client
                        .model()
                        .channel(&folded)
                        .and_then(|channel| channel.log.get_by_key(&key))
                    else {
                        continue;
                    };
                    writeln!(out, "<{}> {}", message.sender, message.text)?;
                    if message.text.contains("hello") && message.sender != client.nick() {
                        client.send_message(&target, format!("hello {}", message.sender));
                    }
                }
                Event::LinkDead => {
                    writeln!(out, "the link died")?;
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}

/// The two clocks the engine wants: one that never goes backward, and the wall clock.
fn now(started: SystemTime) -> Now {
    Now {
        monotonic_ms: millis_since(started),
        unix_ms: millis_since(UNIX_EPOCH),
    }
}

/// Milliseconds from `point` until now, or 0 if the clock has gone backward since.
fn millis_since(point: SystemTime) -> u64 {
    SystemTime::now()
        .duration_since(point)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}
