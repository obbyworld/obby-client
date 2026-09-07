//! A smoke test against a real server.
//!
//! Ignored by default: it needs the network, and a test suite that fails when the wifi drops is a
//! test suite people learn to ignore. Run it deliberately:
//!
//! ```sh
//! cargo test -p obby-client --test live -- --ignored --nocapture
//! ```
//!
//! Everything else in this repository proves the engine against transcripts we wrote. This proves
//! it against a server we did not, which is the only way to find out what a real ObbyIRCd does that
//! our reading of it missed.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use obby_client::{Client, Config, Event, Now, Phase};

const HOST: &str = "irc.h4ks.com";
const PORT: u16 = 6697;
const CHANNEL: &str = "#obby-client-smoke";

/// Give up rather than hang if the server never answers.
const DEADLINE: Duration = Duration::from_secs(45);

struct Transport {
    connection: rustls::ClientConnection,
    socket: TcpStream,
}

impl Transport {
    fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connection = rustls::ClientConnection::new(Arc::new(config), HOST.try_into()?)?;
        let socket = TcpStream::connect((HOST, PORT))?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        Ok(Self { connection, socket })
    }

    /// Move whatever is pending in both directions. Returns the plaintext that arrived.
    fn pump(&mut self, outbound: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if !outbound.is_empty() {
            self.connection.writer().write_all(outbound)?;
        }
        while self.connection.wants_write() {
            self.connection.write_tls(&mut self.socket)?;
        }
        if self.connection.wants_read() {
            match self.connection.read_tls(&mut self.socket) {
                Ok(0) => return Err("the server closed the connection".into()),
                Ok(_) => {
                    self.connection.process_new_packets()?;
                }
                // the read timeout firing only means nothing arrived yet
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut plaintext = Vec::new();
        let _ = self.connection.reader().read_to_end(&mut plaintext);
        Ok(plaintext)
    }
}

fn now(started: Instant) -> Now {
    Now {
        monotonic_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0)),
    }
}

/// What the real server said about us, or nothing when the collector never filled a record.
///
/// A 311 is the one numeric every server sends, so an absent host means nothing collected it.
fn describe_whois(client: &Client, nick: &str) -> Option<String> {
    let whois = client.model().whois(&client.isupport().fold(nick))?;
    whois.host.as_ref()?;
    Some(format!(
        "whois: host {:?}, server {:?}, secure {}, channels {:?}",
        whois.host, whois.server, whois.secure, whois.channels
    ))
}

#[test]
#[ignore = "needs the network"]
fn registers_joins_and_speaks_to_a_real_server() -> Result<(), Box<dyn std::error::Error>> {
    let nick = format!("obbyrs{}", std::process::id() % 10_000);
    let mut config = Config::new(nick.clone());
    config.alt_nicks = vec![format!("{nick}_"), format!("{nick}__")];

    let mut client = Client::new(config);
    let mut transport = Transport::open()?;
    client.handle_connected();

    let started = Instant::now();
    let mut joined = false;
    let mut said = false;
    let mut whois_done = false;
    let mut caps = Vec::new();

    while started.elapsed() < DEADLINE {
        let mut outbound = Vec::new();
        while let Some(bytes) = client.poll_transmit() {
            outbound.extend_from_slice(&bytes);
        }
        let inbound = transport.pump(&outbound)?;
        if !inbound.is_empty() {
            client.handle_bytes(&inbound);
        }
        client.tick(now(started));

        while let Some(event) = client.poll_event() {
            match event {
                Event::CapabilitiesAcknowledged { names } => caps.extend(names),
                Event::Registered { nick } => {
                    println!("registered as {nick}");
                    client.join(CHANNEL, None);
                }
                Event::ModelChanged {
                    change: obby_client::Change::ChannelJoined { channel },
                } if channel == CHANNEL => {
                    joined = true;
                    client.send_message(CHANNEL, "obby-client smoke test");
                }
                Event::ModelChanged {
                    change: obby_client::Change::MessageAdded { target, .. },
                } if target == CHANNEL && !said => {
                    said = true;
                    // the vendor WHOIS batch and the token mint only exist on a real ObbyIRCd, so
                    // this is the only place either handler meets the wire it was written against
                    client.whois(nick.clone());
                    client.generate_token("FILEHOST");
                }
                Event::ModelChanged {
                    change: obby_client::Change::WhoisReceived { .. },
                } => whois_done = true,
                Event::AuthToken {
                    service, endpoint, ..
                } => println!("minted a {service} token for {endpoint}"),
                Event::ServerReply { severity, code, .. } => {
                    println!("server reply: {severity:?} {code}");
                }
                _ => {}
            }
        }

        if joined && said && whois_done {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    println!("negotiated {} capabilities: {caps:?}", caps.len());
    let isupport = client.isupport();
    println!(
        "casemapping {:?}, chantypes accept ^ voice channels: {}",
        isupport.casemapping(),
        isupport.is_channel("^voice")
    );

    assert_eq!(
        client.phase(),
        Phase::Registered,
        "registration did not complete within {DEADLINE:?}"
    );
    assert!(joined, "never joined {CHANNEL}");
    assert!(said, "our own message never came back into the model");
    assert!(whois_done, "the WHOIS reply never finished");

    let whois = describe_whois(&client, &nick).expect("the WHOIS collector filled a record");
    println!("{whois}");
    assert_eq!(
        client.dropped_lines(),
        0,
        "the server sent something we could not parse, which is the whole point of running this"
    );

    let channel = client
        .model()
        .channel(&isupport.fold(CHANNEL))
        .expect("the channel we joined");
    println!(
        "{} has {} members and {} messages",
        channel.name,
        channel.members.len(),
        channel.log.len()
    );

    client.quit(Some("smoke test done".to_string()));
    let mut goodbye = Vec::new();
    while let Some(bytes) = client.poll_transmit() {
        goodbye.extend_from_slice(&bytes);
    }
    let _ = transport.pump(&goodbye);
    Ok(())
}
