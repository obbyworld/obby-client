//! Parsing a MODE change.
//!
//! A mode letter takes an argument or not depending on its class in `CHANMODES` and on whether it is
//! a membership mode from `PREFIX`. Get the arity wrong and every argument after the mistake is
//! attached to the wrong letter, so this is driven entirely by what the server advertised.

use alloc::string::String;
use alloc::vec::Vec;

use crate::isupport::Isupport;

/// One mode letter changing, with the argument it consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct ModeChange {
    /// True for `+`, false for `-`.
    pub set: bool,
    /// The mode letter.
    pub mode: char,
    /// The argument, when this mode takes one in this direction.
    pub arg: Option<String>,
    /// True when this letter grants membership status rather than setting a channel mode, so a host
    /// can route it to the member list instead of the channel.
    pub membership: bool,
}

/// Split a channel MODE change into one entry per letter.
///
/// `params` is everything after the target: the mode string, then its arguments.
///
/// A letter the server never advertised is treated as taking no argument. Consuming one instead
/// would shift every following argument onto the wrong letter, and a missing argument is the
/// cheaper mistake to recover from.
pub fn parse_channel_modes(isupport: &Isupport, params: &[String]) -> Vec<ModeChange> {
    let mut args = params.iter().skip(1);
    let Some(spec) = params.first() else {
        return Vec::new();
    };
    let chanmodes = isupport.chanmodes();
    let prefix = isupport.prefix();

    let mut changes = Vec::new();
    let mut set = true;
    for mode in spec.chars() {
        match mode {
            '+' => set = true,
            '-' => set = false,
            _ => {
                let membership = prefix.is_membership_mode(mode);
                let takes_arg = membership
                    || chanmodes.list.contains(mode)
                    || chanmodes.always_arg.contains(mode)
                    || (set && chanmodes.arg_on_set.contains(mode));
                let arg = takes_arg.then(|| args.next().cloned()).flatten();
                changes.push(ModeChange {
                    set,
                    mode,
                    arg,
                    membership,
                });
            }
        }
    }
    changes
}

/// Split a user MODE change. User modes never take an argument.
pub fn parse_user_modes(spec: &str) -> Vec<ModeChange> {
    let mut changes = Vec::new();
    let mut set = true;
    for mode in spec.chars() {
        match mode {
            '+' => set = true,
            '-' => set = false,
            _ => changes.push(ModeChange {
                set,
                mode,
                arg: None,
                membership: false,
            }),
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use alloc::vec;

    fn server() -> Isupport {
        let mut isupport = Isupport::default();
        isupport.apply("PREFIX=(qaohv)~&@%+");
        isupport.apply("CHANMODES=beI,k,l,imnpst");
        isupport
    }

    fn params(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn parse(items: &[&str]) -> Vec<ModeChange> {
        parse_channel_modes(&server(), &params(items))
    }

    #[test]
    fn a_flag_takes_no_argument() {
        assert_eq!(
            parse(&["+m"]),
            vec![ModeChange {
                set: true,
                mode: 'm',
                arg: None,
                membership: false
            }]
        );
    }

    #[test]
    fn a_list_mode_takes_an_argument_both_ways() {
        let changes = parse(&["+b-b", "a!*@*", "b!*@*"]);
        assert_eq!(changes[0].arg.as_deref(), Some("a!*@*"));
        assert!(changes[0].set);
        assert_eq!(changes[1].arg.as_deref(), Some("b!*@*"));
        assert!(!changes[1].set);
    }

    #[test]
    fn a_limit_takes_an_argument_only_when_set() {
        assert_eq!(parse(&["+l", "50"])[0].arg.as_deref(), Some("50"));
        assert_eq!(parse(&["-l"])[0].arg, None);
    }

    #[test]
    fn a_key_takes_an_argument_both_ways() {
        assert_eq!(parse(&["+k", "secret"])[0].arg.as_deref(), Some("secret"));
        assert_eq!(parse(&["-k", "secret"])[0].arg.as_deref(), Some("secret"));
    }

    #[test]
    fn membership_modes_are_flagged_and_take_a_nick() {
        let changes = parse(&["+ov-v", "alice", "bob", "carol"]);
        assert_eq!(
            changes[0],
            ModeChange {
                set: true,
                mode: 'o',
                arg: Some("alice".to_owned()),
                membership: true
            }
        );
        assert_eq!(changes[1].arg.as_deref(), Some("bob"));
        assert_eq!(
            changes[2],
            ModeChange {
                set: false,
                mode: 'v',
                arg: Some("carol".to_owned()),
                membership: true
            }
        );
    }

    #[test]
    fn mixes_arities_in_one_line_without_shifting_arguments() {
        let changes = parse(&["+ontl-k", "alice", "42", "oldkey"]);
        assert_eq!(
            changes[0].arg.as_deref(),
            Some("alice"),
            "o consumes a nick"
        );
        assert_eq!(changes[1].arg, None, "n is a flag");
        assert_eq!(changes[2].arg, None, "t is a flag");
        assert_eq!(
            changes[3].arg.as_deref(),
            Some("42"),
            "l consumes a limit when set"
        );
        assert_eq!(
            changes[4].arg.as_deref(),
            Some("oldkey"),
            "k consumes a key when unset"
        );
    }

    #[test]
    fn an_unknown_mode_consumes_nothing() {
        let changes = parse(&["+Zo", "alice"]);
        assert_eq!(changes[0].arg, None);
        assert_eq!(
            changes[1].arg.as_deref(),
            Some("alice"),
            "the nick still lands on o"
        );
    }

    #[test]
    fn a_missing_argument_leaves_the_mode_without_one() {
        let changes = parse(&["+ov", "alice"]);
        assert_eq!(changes[0].arg.as_deref(), Some("alice"));
        assert_eq!(changes[1].arg, None);
    }

    #[test]
    fn arity_follows_the_advertised_classes_not_a_hardcoded_table() {
        let mut isupport = Isupport::default();
        // a server that makes the usual flag `n` an argument mode instead
        isupport.apply("CHANMODES=b,n,l,impst");
        let changes = parse_channel_modes(&isupport, &params(&["+n", "value"]));
        assert_eq!(changes[0].arg.as_deref(), Some("value"));
    }

    #[test]
    fn an_empty_change_yields_nothing() {
        assert!(parse(&[]).is_empty());
        assert!(parse(&["+"]).is_empty());
    }

    #[test]
    fn user_modes_never_take_arguments() {
        assert_eq!(
            parse_user_modes("+iw-o"),
            vec![
                ModeChange {
                    set: true,
                    mode: 'i',
                    arg: None,
                    membership: false
                },
                ModeChange {
                    set: true,
                    mode: 'w',
                    arg: None,
                    membership: false
                },
                ModeChange {
                    set: false,
                    mode: 'o',
                    arg: None,
                    membership: false
                },
            ]
        );
    }
}
