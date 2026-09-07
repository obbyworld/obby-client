//! Conformance vectors for splitting a `nick!user@host` source into its parts.
//!
//! Generated from ircdocs/parser-tests, `tests/userhost-split.yaml`:
//! <https://raw.githubusercontent.com/ircdocs/parser-tests/master/tests/userhost-split.yaml>
//! Do not hand edit; regenerate from the upstream file if the vectors change.

use obby_proto::Source;

struct Case {
    source: &'static str,
    nick: &'static str,
    user: &'static str,
    host: &'static str,
}

const CASES: &[Case] = &[
    Case {
        source: "coolguy",
        nick: "coolguy",
        user: "",
        host: "",
    },
    Case {
        source: "coolguy!ag@127.0.0.1",
        nick: "coolguy",
        user: "ag",
        host: "127.0.0.1",
    },
    Case {
        source: "coolguy!~ag@localhost",
        nick: "coolguy",
        user: "~ag",
        host: "localhost",
    },
    Case {
        source: "coolguy@127.0.0.1",
        nick: "coolguy",
        user: "",
        host: "127.0.0.1",
    },
    Case {
        source: "coolguy!ag",
        nick: "coolguy",
        user: "ag",
        host: "",
    },
    Case {
        source: "coolguy!ag@net\u{3}5w\u{3}ork.admin",
        nick: "coolguy",
        user: "ag",
        host: "net\u{3}5w\u{3}ork.admin",
    },
    Case {
        source: "coolguy!~ag@n\u{2}et\u{3}05w\u{f}ork.admin",
        nick: "coolguy",
        user: "~ag",
        host: "n\u{2}et\u{3}05w\u{f}ork.admin",
    },
];

#[test]
fn userhost_split_vectors() {
    for case in CASES {
        let source = Source::parse(case.source);
        assert_eq!(
            source.name, case.nick,
            "nick mismatch for {:?}",
            case.source
        );
        assert_eq!(
            source.user.as_deref().unwrap_or(""),
            case.user,
            "user mismatch for {:?}",
            case.source
        );
        assert_eq!(
            source.host.as_deref().unwrap_or(""),
            case.host,
            "host mismatch for {:?}",
            case.source
        );
    }
}
