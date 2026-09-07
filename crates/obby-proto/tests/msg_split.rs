//! Conformance vectors for splitting a wire line into tags, source, verb and params.
//!
//! Generated from ircdocs/parser-tests, `tests/msg-split.yaml`:
//! <https://raw.githubusercontent.com/ircdocs/parser-tests/master/tests/msg-split.yaml>
//! Do not hand edit; regenerate from the upstream file if the vectors change.

use std::collections::HashSet;

use obby_proto::Message;

struct Case {
    input: &'static str,
    tags: Option<&'static [(&'static str, &'static str)]>,
    source: Option<&'static str>,
    verb: &'static str,
    params: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        input: "foo bar baz asdf",
        tags: None,
        source: None,
        verb: "foo",
        params: &["bar", "baz", "asdf"],
    },
    Case {
        input: ":coolguy foo bar baz asdf",
        tags: None,
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "asdf"],
    },
    Case {
        input: "foo bar baz :asdf quux",
        tags: None,
        source: None,
        verb: "foo",
        params: &["bar", "baz", "asdf quux"],
    },
    Case {
        input: "foo bar baz :",
        tags: None,
        source: None,
        verb: "foo",
        params: &["bar", "baz", ""],
    },
    Case {
        input: "foo bar baz ::asdf",
        tags: None,
        source: None,
        verb: "foo",
        params: &["bar", "baz", ":asdf"],
    },
    Case {
        input: ":coolguy foo bar baz :asdf quux",
        tags: None,
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "asdf quux"],
    },
    Case {
        input: ":coolguy foo bar baz :  asdf quux ",
        tags: None,
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  asdf quux "],
    },
    Case {
        input: ":coolguy PRIVMSG bar :lol :) ",
        tags: None,
        source: Some("coolguy"),
        verb: "PRIVMSG",
        params: &["bar", "lol :) "],
    },
    Case {
        input: ":coolguy foo bar baz :",
        tags: None,
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", ""],
    },
    Case {
        input: ":coolguy foo bar baz :  ",
        tags: None,
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  "],
    },
    Case {
        input: "@a=b;c=32;k;rt=ql7 foo",
        tags: Some(&[("a", "b"), ("c", "32"), ("k", ""), ("rt", "ql7")]),
        source: None,
        verb: "foo",
        params: &[],
    },
    Case {
        input: "@a=b\\\\and\\nk;c=72\\s45;d=gh\\:764 foo",
        tags: Some(&[("a", "b\\and\nk"), ("c", "72 45"), ("d", "gh;764")]),
        source: None,
        verb: "foo",
        params: &[],
    },
    Case {
        input: "@c;h=;a=b :quux ab cd",
        tags: Some(&[("a", "b"), ("c", ""), ("h", "")]),
        source: Some("quux"),
        verb: "ab",
        params: &["cd"],
    },
    Case {
        input: ":src JOIN #chan",
        tags: None,
        source: Some("src"),
        verb: "JOIN",
        params: &["#chan"],
    },
    Case {
        input: ":src JOIN :#chan",
        tags: None,
        source: Some("src"),
        verb: "JOIN",
        params: &["#chan"],
    },
    Case {
        input: ":src AWAY",
        tags: None,
        source: Some("src"),
        verb: "AWAY",
        params: &[],
    },
    Case {
        input: ":src AWAY ",
        tags: None,
        source: Some("src"),
        verb: "AWAY",
        params: &[],
    },
    Case {
        input: ":cool\tguy foo bar baz",
        tags: None,
        source: Some("cool\tguy"),
        verb: "foo",
        params: &["bar", "baz"],
    },
    Case {
        input: ":coolguy!ag@net\u{3}5w\u{3}ork.admin PRIVMSG foo :bar baz",
        tags: None,
        source: Some("coolguy!ag@net\u{3}5w\u{3}ork.admin"),
        verb: "PRIVMSG",
        params: &["foo", "bar baz"],
    },
    Case {
        input: ":coolguy!~ag@n\u{2}et\u{3}05w\u{f}ork.admin PRIVMSG foo :bar baz",
        tags: None,
        source: Some("coolguy!~ag@n\u{2}et\u{3}05w\u{f}ork.admin"),
        verb: "PRIVMSG",
        params: &["foo", "bar baz"],
    },
    Case {
        input: "@tag1=value1;tag2;vendor1/tag3=value2;vendor2/tag4= :irc.example.com COMMAND param1 param2 :param3 param3",
        tags: Some(&[
            ("tag1", "value1"),
            ("tag2", ""),
            ("vendor1/tag3", "value2"),
            ("vendor2/tag4", ""),
        ]),
        source: Some("irc.example.com"),
        verb: "COMMAND",
        params: &["param1", "param2", "param3 param3"],
    },
    Case {
        input: ":irc.example.com COMMAND param1 param2 :param3 param3",
        tags: None,
        source: Some("irc.example.com"),
        verb: "COMMAND",
        params: &["param1", "param2", "param3 param3"],
    },
    Case {
        input: "@tag1=value1;tag2;vendor1/tag3=value2;vendor2/tag4 COMMAND param1 param2 :param3 param3",
        tags: Some(&[
            ("tag1", "value1"),
            ("tag2", ""),
            ("vendor1/tag3", "value2"),
            ("vendor2/tag4", ""),
        ]),
        source: None,
        verb: "COMMAND",
        params: &["param1", "param2", "param3 param3"],
    },
    Case {
        input: "COMMAND",
        tags: None,
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: "@foo=\\\\\\\\\\:\\\\s\\s\\r\\n COMMAND",
        tags: Some(&[("foo", "\\\\;\\s \r\n")]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: ":gravel.mozilla.org 432  #momo :Erroneous Nickname: Illegal characters",
        tags: None,
        source: Some("gravel.mozilla.org"),
        verb: "432",
        params: &["#momo", "Erroneous Nickname: Illegal characters"],
    },
    Case {
        input: ":gravel.mozilla.org MODE #tckk +n ",
        tags: None,
        source: Some("gravel.mozilla.org"),
        verb: "MODE",
        params: &["#tckk", "+n"],
    },
    Case {
        input: ":services.esper.net MODE #foo-bar +o foobar  ",
        tags: None,
        source: Some("services.esper.net"),
        verb: "MODE",
        params: &["#foo-bar", "+o", "foobar"],
    },
    Case {
        input: "@tag1=value\\\\ntest COMMAND",
        tags: Some(&[("tag1", "value\\ntest")]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: "@tag1=value\\1 COMMAND",
        tags: Some(&[("tag1", "value1")]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: "@tag1=value1\\ COMMAND",
        tags: Some(&[("tag1", "value1")]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: "@tag1=1;tag2=3;tag3=4;tag1=5 COMMAND",
        tags: Some(&[("tag1", "5"), ("tag2", "3"), ("tag3", "4")]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: "@tag1=1;tag2=3;tag3=4;tag1=5;vendor/tag2=8 COMMAND",
        tags: Some(&[
            ("tag1", "5"),
            ("tag2", "3"),
            ("tag3", "4"),
            ("vendor/tag2", "8"),
        ]),
        source: None,
        verb: "COMMAND",
        params: &[],
    },
    Case {
        input: ":SomeOp MODE #channel :+i",
        tags: None,
        source: Some("SomeOp"),
        verb: "MODE",
        params: &["#channel", "+i"],
    },
    Case {
        input: ":SomeOp MODE #channel +oo SomeUser :AnotherUser",
        tags: None,
        source: Some("SomeOp"),
        verb: "MODE",
        params: &["#channel", "+oo", "SomeUser", "AnotherUser"],
    },
];

#[test]
fn msg_split_vectors() {
    for case in CASES {
        let msg = match Message::parse(case.input) {
            Ok(msg) => msg,
            Err(err) => panic!("failed to parse {:?}: {err}", case.input),
        };

        assert!(
            msg.is(case.verb),
            "verb mismatch for {:?}: got {:?}",
            case.input,
            msg.command
        );

        if let Some(expected) = case.source {
            let actual = msg.source.as_ref().map(ToString::to_string);
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "source mismatch for {:?}",
                case.input
            );
        } else {
            assert!(
                msg.source.is_none(),
                "expected no source for {:?}",
                case.input
            );
        }

        let params: Vec<&str> = msg.params.iter().map(String::as_str).collect();
        assert_eq!(params, case.params, "params mismatch for {:?}", case.input);

        if let Some(expected) = case.tags {
            let keys: HashSet<&str> = msg.tags.iter().map(|tag| tag.key.as_str()).collect();
            assert_eq!(
                keys.len(),
                expected.len(),
                "tag count mismatch for {:?}",
                case.input
            );
            for &(key, value) in expected {
                assert_eq!(
                    msg.tag(key).unwrap_or(""),
                    value,
                    "tag {key} mismatch for {:?}",
                    case.input
                );
            }
        } else {
            assert!(msg.tags.is_empty(), "expected no tags for {:?}", case.input);
        }
    }
}
