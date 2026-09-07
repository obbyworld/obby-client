//! mIRC formatting codes and CTCP framing inside a message body.
//!
//! `PRIVMSG`/`NOTICE` bodies are plain bytes with a handful of control characters mixed in: toggles
//! for bold and friends, `\x03`/`\x04` colour introducers, and the `\x01`-delimited CTCP wrapper. None
//! of it is escaped, so a colour code's digits are only ambiguous, never invalid, and a parser has to
//! pick one reading rather than reject the line.

use alloc::string::String;
use alloc::vec::Vec;
use core::iter::Peekable;
use core::mem;
use core::str::Chars;

const BOLD: char = '\u{02}';
const ITALIC: char = '\u{1D}';
const UNDERLINE: char = '\u{1F}';
const STRIKETHROUGH: char = '\u{1E}';
const MONOSPACE: char = '\u{11}';
const REVERSE: char = '\u{16}';
const RESET: char = '\u{0F}';
const COLOUR: char = '\u{03}';
const HEX_COLOUR: char = '\u{04}';
const CTCP_DELIM: char = '\u{01}';

/// A colour carried by `\x03` or `\x04`.
///
/// Both introducers are kept as one type because either can appear as a foreground or a background,
/// and a consumer rendering a [`Style`] does not care which byte put it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub enum Colour {
    /// One of mIRC's 99 numbered colours (`\x03`), `0` through `99`.
    Numbered(u8),
    /// A 24-bit colour (`\x04`), as `(red, green, blue)`.
    Hex(u8, u8, u8),
}

/// The six independent text-decoration toggles, packed into one byte.
///
/// mIRC turns each on and off independently of the others, so this is a bitset rather than the six
/// separate `bool` fields that would trip `clippy::struct_excessive_bools`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Emphasis(u8);

impl Emphasis {
    const BOLD: u8 = 0b0000_0001;
    const ITALIC: u8 = 0b0000_0010;
    const UNDERLINE: u8 = 0b0000_0100;
    const STRIKETHROUGH: u8 = 0b0000_1000;
    const MONOSPACE: u8 = 0b0001_0000;
    const REVERSE: u8 = 0b0010_0000;

    /// True when bold (`\x02`) is active.
    pub fn bold(self) -> bool {
        self.0 & Self::BOLD != 0
    }

    /// True when italic (`\x1D`) is active.
    pub fn italic(self) -> bool {
        self.0 & Self::ITALIC != 0
    }

    /// True when underline (`\x1F`) is active.
    pub fn underline(self) -> bool {
        self.0 & Self::UNDERLINE != 0
    }

    /// True when strikethrough (`\x1E`) is active.
    pub fn strikethrough(self) -> bool {
        self.0 & Self::STRIKETHROUGH != 0
    }

    /// True when monospace (`\x11`) is active.
    pub fn monospace(self) -> bool {
        self.0 & Self::MONOSPACE != 0
    }

    /// True when reverse video (`\x16`) is active. Carried through rather than dropped: the reference
    /// client emits this byte but never draws it.
    pub fn reverse(self) -> bool {
        self.0 & Self::REVERSE != 0
    }

    fn toggle(&mut self, bit: u8) {
        self.0 ^= bit;
    }
}

/// The formatting active over a run of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Style {
    /// The active bold/italic/underline/strikethrough/monospace/reverse toggles.
    pub emphasis: Emphasis,
    /// The active foreground, if a colour code set one.
    pub foreground: Option<Colour>,
    /// The active background, if a colour code set one.
    pub background: Option<Colour>,
}

/// A run of text plus the [`Style`] active over it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Span {
    /// The text, with every control code removed.
    pub text: String,
    /// The formatting active while this text was written.
    pub style: Style,
}

/// A CTCP request or reply extracted from a message body, such as `ACTION waves`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Ctcp {
    /// The command word, such as `ACTION` or `VERSION`.
    pub command: String,
    /// Everything after the command, still carrying any formatting codes of its own.
    pub params: String,
}

/// Recognise a CTCP-wrapped body: `\x01COMMAND params\x01`.
///
/// The reference client slices `ACTION` out with a hand-rolled prefix check and renders the result
/// without ever running it back through formatting. Returning `params` untouched here means a caller
/// can hand it straight to [`parse_spans`] instead.
///
/// The closing `\x01` is optional: some clients drop it, and a body missing one still names a real
/// command.
pub fn parse_ctcp(body: &str) -> Option<Ctcp> {
    let inner = body.strip_prefix(CTCP_DELIM)?;
    let inner = inner.strip_suffix(CTCP_DELIM).unwrap_or(inner);
    let (command, params) = inner.split_once(' ').unwrap_or((inner, ""));
    Some(Ctcp {
        command: command.into(),
        params: params.into(),
    })
}

/// Parse a message body into styled spans.
///
/// A malformed or unterminated colour sequence consumes only what it can validly read and leaves the
/// rest as text, so no byte of the original body is ever dropped.
pub fn parse_spans(body: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut current = String::new();
    let mut chars = body.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            BOLD => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::BOLD);
            }
            ITALIC => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::ITALIC);
            }
            UNDERLINE => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::UNDERLINE);
            }
            STRIKETHROUGH => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::STRIKETHROUGH);
            }
            MONOSPACE => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::MONOSPACE);
            }
            REVERSE => {
                flush(&mut spans, &mut current, &style);
                style.emphasis.toggle(Emphasis::REVERSE);
            }
            RESET => {
                flush(&mut spans, &mut current, &style);
                style = Style::default();
            }
            COLOUR => {
                flush(&mut spans, &mut current, &style);
                apply_colour(&mut style, parse_numbered_colour(&mut chars));
            }
            HEX_COLOUR => {
                flush(&mut spans, &mut current, &style);
                apply_colour(&mut style, parse_hex_colour(&mut chars));
            }
            _ => current.push(c),
        }
    }
    flush(&mut spans, &mut current, &style);
    spans
}

/// The plain text of a message body, with every formatting and colour code removed.
pub fn strip_formatting(body: &str) -> String {
    parse_spans(body)
        .into_iter()
        .map(|span| span.text)
        .collect()
}

fn flush(spans: &mut Vec<Span>, current: &mut String, style: &Style) {
    if current.is_empty() {
        return;
    }
    spans.push(Span {
        text: mem::take(current),
        style: *style,
    });
}

fn apply_colour(style: &mut Style, parsed: Option<(Colour, Option<Colour>)>) {
    // a colour code with no digits after it resets colour, same as a bare `\x0F` for style
    if let Some((foreground, background)) = parsed {
        style.foreground = Some(foreground);
        if let Some(background) = background {
            style.background = Some(background);
        }
    } else {
        style.foreground = None;
        style.background = None;
    }
}

/// `None` means the code had no digits at all, which mIRC treats as a colour reset. Otherwise the
/// foreground is always present; the background is only set when a comma is immediately followed by
/// a digit, so a bare trailing comma is left as ordinary text.
fn parse_numbered_colour(chars: &mut Peekable<Chars<'_>>) -> Option<(Colour, Option<Colour>)> {
    let foreground = take_digits(chars, 2)?;
    let background = take_comma_digits(chars);
    Some((
        Colour::Numbered(foreground),
        background.map(Colour::Numbered),
    ))
}

/// Same shape as [`parse_numbered_colour`], but each half needs exactly six hex digits: a short or
/// broken run is not a partial colour, it is text that happens to start with hex digits.
fn parse_hex_colour(chars: &mut Peekable<Chars<'_>>) -> Option<(Colour, Option<Colour>)> {
    let foreground = take_hex_triple(chars)?;
    let background = take_comma_hex_triple(chars);
    Some((
        Colour::Hex(foreground.0, foreground.1, foreground.2),
        background,
    ))
}

/// Take up to `max` ASCII digits, greedily. A colour code followed by three digits always claims the
/// first two: mIRC has no way to say "this one-digit colour is followed by a literal digit," so every
/// implementation reads the maximum it can.
fn take_digits(chars: &mut Peekable<Chars<'_>>, max: u8) -> Option<u8> {
    let mut probe = chars.clone();
    let mut value: u8 = 0;
    let mut count = 0u8;
    while count < max {
        let Some(digit) = probe.peek().and_then(|c| c.to_digit(10)) else {
            break;
        };
        value = value * 10 + u8::try_from(digit).unwrap_or_default();
        probe.next();
        count += 1;
    }
    if count == 0 {
        return None;
    }
    *chars = probe;
    Some(value)
}

fn take_comma_digits(chars: &mut Peekable<Chars<'_>>) -> Option<u8> {
    let mut probe = chars.clone();
    if probe.next() != Some(',') {
        return None;
    }
    let value = take_digits(&mut probe, 2)?;
    *chars = probe;
    Some(value)
}

fn hex_byte(chars: &mut Peekable<Chars<'_>>) -> Option<u8> {
    let hi = u8::try_from(chars.next()?.to_digit(16)?).unwrap_or_default();
    let lo = u8::try_from(chars.next()?.to_digit(16)?).unwrap_or_default();
    Some((hi << 4) | lo)
}

fn take_hex_triple(chars: &mut Peekable<Chars<'_>>) -> Option<(u8, u8, u8)> {
    let mut probe = chars.clone();
    let triple = (
        hex_byte(&mut probe)?,
        hex_byte(&mut probe)?,
        hex_byte(&mut probe)?,
    );
    *chars = probe;
    Some(triple)
}

fn take_comma_hex_triple(chars: &mut Peekable<Chars<'_>>) -> Option<Colour> {
    let mut probe = chars.clone();
    if probe.next() != Some(',') {
        return None;
    }
    let (r, g, b) = take_hex_triple(&mut probe)?;
    *chars = probe;
    Some(Colour::Hex(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn plain(text: &str) -> Span {
        Span {
            text: text.into(),
            style: Style::default(),
        }
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        assert_eq!(parse_spans("hello world"), vec![plain("hello world")]);
        assert_eq!(strip_formatting("hello world"), "hello world");
    }

    #[test]
    fn bold_wraps_the_text_between_toggles() {
        let spans = parse_spans("a\u{02}b\u{02}c");
        assert_eq!(spans[0], plain("a"));
        assert!(spans[1].style.emphasis.bold());
        assert_eq!(spans[1].text, "b");
        assert!(!spans[2].style.emphasis.bold());
        assert_eq!(spans[2].text, "c");
    }

    #[test]
    fn italic_underline_strikethrough_monospace_and_reverse_each_toggle_their_own_flag() {
        let spans = parse_spans("\u{1D}i\u{1F}u\u{1E}s\u{11}m\u{16}r");
        assert!(spans[0].style.emphasis.italic());
        assert!(spans[1].style.emphasis.italic() && spans[1].style.emphasis.underline());
        assert!(spans[2].style.emphasis.strikethrough());
        assert!(spans[3].style.emphasis.monospace());
        assert!(spans[4].style.emphasis.reverse());
    }

    #[test]
    fn reverse_is_kept_not_dropped() {
        let spans = parse_spans("\u{16}flipped");
        assert!(
            spans[0].style.emphasis.reverse(),
            "reverse must survive into the span"
        );
    }

    #[test]
    fn a_colour_code_with_one_digit_sets_only_the_foreground() {
        let spans = parse_spans("\u{03}4red");
        assert_eq!(spans[0].style.foreground, Some(Colour::Numbered(4)));
        assert_eq!(spans[0].style.background, None);
    }

    #[test]
    fn a_colour_code_with_a_background_sets_both() {
        let spans = parse_spans("\u{03}4,8text");
        assert_eq!(spans[0].style.foreground, Some(Colour::Numbered(4)));
        assert_eq!(spans[0].style.background, Some(Colour::Numbered(8)));
    }

    #[test]
    fn a_colour_code_stops_at_two_digits() {
        let spans = parse_spans("\u{03}123abc");
        assert_eq!(spans[0].style.foreground, Some(Colour::Numbered(12)));
        assert_eq!(spans[0].text, "3abc");
    }

    #[test]
    fn a_comma_not_followed_by_a_digit_is_left_as_text() {
        let spans = parse_spans("\u{03}4,hi");
        assert_eq!(spans[0].style.foreground, Some(Colour::Numbered(4)));
        assert_eq!(spans[0].style.background, None);
        assert_eq!(spans[0].text, ",hi");
    }

    #[test]
    fn a_bare_colour_code_resets_colour() {
        let spans = parse_spans("\u{03}4,8a\u{03}b");
        assert_eq!(spans[1].style.foreground, None);
        assert_eq!(spans[1].style.background, None);
        assert_eq!(spans[1].text, "b");
    }

    #[test]
    fn hex_colour_reads_six_digits_per_half() {
        let spans = parse_spans("\u{04}FF00AAtext");
        assert_eq!(
            spans[0].style.foreground,
            Some(Colour::Hex(0xFF, 0x00, 0xAA))
        );
        assert_eq!(spans[0].text, "text");
    }

    #[test]
    fn hex_colour_with_a_background() {
        let spans = parse_spans("\u{04}FF00AA,00FF00text");
        assert_eq!(
            spans[0].style.foreground,
            Some(Colour::Hex(0xFF, 0x00, 0xAA))
        );
        assert_eq!(
            spans[0].style.background,
            Some(Colour::Hex(0x00, 0xFF, 0x00))
        );
    }

    #[test]
    fn a_short_hex_run_is_not_a_colour() {
        let spans = parse_spans("\u{04}FF0text");
        assert_eq!(spans[0].style.foreground, None);
        assert_eq!(spans[0].text, "FF0text");
    }

    #[test]
    fn reset_clears_every_flag_and_both_colours() {
        let spans = parse_spans("\u{02}\u{03}4,8bold-red\u{0F}plain");
        assert!(spans[0].style.emphasis.bold());
        assert_eq!(spans[0].style.foreground, Some(Colour::Numbered(4)));
        assert_eq!(spans[1].style, Style::default());
        assert_eq!(spans[1].text, "plain");
    }

    #[test]
    fn styles_nest_and_unwind_independently() {
        let spans = parse_spans("\u{02}bold\u{1D}bold-italic\u{02}italic-only");
        assert!(spans[0].style.emphasis.bold() && !spans[0].style.emphasis.italic());
        assert!(spans[1].style.emphasis.bold() && spans[1].style.emphasis.italic());
        assert!(!spans[2].style.emphasis.bold() && spans[2].style.emphasis.italic());
    }

    #[test]
    fn an_unterminated_colour_code_does_not_panic_or_lose_text() {
        assert_eq!(strip_formatting("text\u{03}"), "text");
        assert_eq!(strip_formatting("text\u{03}5"), "text");
        assert_eq!(parse_spans("text\u{03}5"), vec![plain("text")]);
    }

    #[test]
    fn a_ctcp_action_keeps_its_formatting_parseable() {
        let ctcp = parse_ctcp("\u{01}ACTION waves \u{02}hi\u{02}\u{01}").expect("ctcp body");
        assert_eq!(ctcp.command, "ACTION");
        assert_eq!(ctcp.params, "waves \u{02}hi\u{02}");
        let spans = parse_spans(&ctcp.params);
        assert_eq!(spans[0], plain("waves "));
        assert!(spans[1].style.emphasis.bold());
        assert_eq!(spans[1].text, "hi");
    }

    #[test]
    fn a_ctcp_body_without_a_closing_delimiter_still_parses() {
        let ctcp = parse_ctcp("\u{01}VERSION").expect("ctcp body");
        assert_eq!(ctcp.command, "VERSION");
        assert_eq!(ctcp.params, "");
    }

    #[test]
    fn a_non_ctcp_body_is_not_recognised() {
        assert_eq!(parse_ctcp("hello"), None);
        assert_eq!(parse_ctcp(""), None);
    }
}
