//! Conformance vectors for rendering tags, source, verb and params into a wire line.
//!
//! Generated from ircdocs/parser-tests, `tests/msg-join.yaml`:
//! <https://raw.githubusercontent.com/ircdocs/parser-tests/master/tests/msg-join.yaml>
//! Do not hand edit; regenerate from the upstream file if the vectors change.

use obby_proto::{Message, Source, Tag, Tags};

struct Case {
    desc: &'static str,
    tags: &'static [(&'static str, &'static str)],
    source: Option<&'static str>,
    verb: &'static str,
    params: &'static [&'static str],
    matches: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        desc: "Simple test with verb and params.",
        tags: &[],
        source: None,
        verb: "foo",
        params: &["bar", "baz", "asdf"],
        matches: &["foo bar baz asdf", "foo bar baz :asdf"],
    },
    Case {
        desc: "Simple test with source and no params.",
        tags: &[],
        source: Some("src"),
        verb: "AWAY",
        params: &[],
        matches: &[":src AWAY"],
    },
    Case {
        desc: "Simple test with source and empty trailing param.",
        tags: &[],
        source: Some("src"),
        verb: "AWAY",
        params: &[""],
        matches: &[":src AWAY :"],
    },
    Case {
        desc: "Simple test with source.",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "asdf"],
        matches: &[":coolguy foo bar baz asdf", ":coolguy foo bar baz :asdf"],
    },
    Case {
        desc: "Simple test with trailing param.",
        tags: &[],
        source: None,
        verb: "foo",
        params: &["bar", "baz", "asdf quux"],
        matches: &["foo bar baz :asdf quux"],
    },
    Case {
        desc: "Simple test with empty trailing param.",
        tags: &[],
        source: None,
        verb: "foo",
        params: &["bar", "baz", ""],
        matches: &["foo bar baz :"],
    },
    Case {
        desc: "Simple test with trailing param containing colon.",
        tags: &[],
        source: None,
        verb: "foo",
        params: &["bar", "baz", ":asdf"],
        matches: &["foo bar baz ::asdf"],
    },
    Case {
        desc: "Test with source and trailing param.",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "asdf quux"],
        matches: &[":coolguy foo bar baz :asdf quux"],
    },
    Case {
        desc: "Test with trailing containing beginning+end whitespace.",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  asdf quux "],
        matches: &[":coolguy foo bar baz :  asdf quux "],
    },
    Case {
        desc: "Test with trailing containing what looks like another trailing param.",
        tags: &[],
        source: Some("coolguy"),
        verb: "PRIVMSG",
        params: &["bar", "lol :) "],
        matches: &[":coolguy PRIVMSG bar :lol :) "],
    },
    Case {
        desc: "Simple test with source and empty trailing.",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", ""],
        matches: &[":coolguy foo bar baz :"],
    },
    Case {
        desc: "Trailing contains only spaces.",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  "],
        matches: &[":coolguy foo bar baz :  "],
    },
    Case {
        desc: "Param containing tab (tab is not considered SPACE for message splitting).",
        tags: &[],
        source: Some("coolguy"),
        verb: "foo",
        params: &["b\tar", "baz"],
        matches: &[":coolguy foo b\tar baz", ":coolguy foo b\tar :baz"],
    },
    Case {
        desc: "Tag with empty value and space-filled trailing.",
        tags: &[("asd", "")],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  "],
        matches: &[
            "@asd :coolguy foo bar baz :  ",
            "@asd= :coolguy foo bar baz :  ",
        ],
    },
    Case {
        desc: "Tag with no value and space-filled trailing.",
        tags: &[("asd", "")],
        source: Some("coolguy"),
        verb: "foo",
        params: &["bar", "baz", "  "],
        matches: &["@asd :coolguy foo bar baz :  "],
    },
    Case {
        desc: "Tags with escaped values.",
        tags: &[("a", "b\\and\nk"), ("d", "gh;764")],
        source: None,
        verb: "foo",
        params: &[],
        matches: &[
            "@a=b\\\\and\\nk;d=gh\\:764 foo",
            "@d=gh\\:764;a=b\\\\and\\nk foo",
        ],
    },
    Case {
        desc: "Tags with escaped values and params.",
        tags: &[("a", "b\\and\nk"), ("d", "gh;764")],
        source: None,
        verb: "foo",
        params: &["par1", "par2"],
        matches: &[
            "@a=b\\\\and\\nk;d=gh\\:764 foo par1 par2",
            "@a=b\\\\and\\nk;d=gh\\:764 foo par1 :par2",
            "@d=gh\\:764;a=b\\\\and\\nk foo par1 par2",
            "@d=gh\\:764;a=b\\\\and\\nk foo par1 :par2",
        ],
    },
    Case {
        desc: "Tag with long, strange values (including LF and newline).",
        tags: &[("foo", "\\\\;\\s \r\n")],
        source: None,
        verb: "COMMAND",
        params: &[],
        matches: &["@foo=\\\\\\\\\\:\\\\s\\s\\r\\n COMMAND"],
    },
];

#[test]
fn msg_join_vectors() {
    for case in CASES {
        let mut msg = Message::new(case.verb, case.params.iter().copied());
        msg.source = case.source.map(Source::parse);
        msg.tags = case
            .tags
            .iter()
            .map(|(key, value)| {
                if value.is_empty() {
                    Tag::flag(*key)
                } else {
                    Tag::new(*key, *value)
                }
            })
            .collect::<Tags>();

        let rendered = msg.to_string();
        assert!(
            case.matches.contains(&rendered.as_str()),
            "{}: rendered {rendered:?}, expected one of {:?}",
            case.desc,
            case.matches
        );
    }
}
