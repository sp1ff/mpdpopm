// Copyright (C) 2020-2025 Michael herstine <sp1ff@pobox.com>
//
// This file is part of mpdpopm.
//
// mpdpopm is free software: you can redistribute it and/or modify it under the terms of the GNU
// General Public License as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// mpdpopm is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even
// the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General
// Public License for more details.
//
// You should have received a copy of the GNU General Public License along with mpdpopm.  If not,
// see <http://www.gnu.org/licenses/>.

//! Types for building the Abstract Syntax Tree when parsing filters
//!
//! This module provides support for our [lalrpop](https://github.com/lalrpop/lalrpop) grammar.

use crate::clients::Client;

use backtrace::Backtrace;
use boolinator::Boolinator;
use chrono::prelude::*;
use tracing::debug;

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

/// The operations that can appear in a filter term
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum OpCode {
    Equality,
    Inequality,
    Contains,
    RegexMatch,
    RegexExclude,
    GreaterThan,
    LessThan,
    GreaterThanEqual,
    LessThanEqual,
}

impl std::fmt::Display for OpCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                OpCode::Equality => "==",
                OpCode::Inequality => "!=",
                OpCode::Contains => "contains",
                OpCode::RegexMatch => "=~",
                OpCode::RegexExclude => "!~",
                OpCode::GreaterThan => ">",
                OpCode::LessThan => "<",
                OpCode::GreaterThanEqual => ">=",
                OpCode::LessThanEqual => "<=",
            }
        )
    }
}

/// The song attributes that can appear on the LHS of a filter term
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Selector {
    Artist,
    Album,
    AlbumArtist,
    Title,
    Track,
    Name,
    Genre,
    Date,
    OriginalDate,
    Composer,
    Performer,
    Conductor,
    Work,
    Grouping,
    Comment,
    Disc,
    Label,
    MusicbrainzAristID,
    MusicbrainzAlbumID,
    MusicbrainzAlbumArtistID,
    MusicbrainzTrackID,
    MusicbrainzReleaseTrackID,
    MusicbrainzWorkID,
    File,
    Base,
    ModifiedSince,
    AudioFormat,
    Rating,
    PlayCount,
    LastPlayed,
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Selector::Artist => "artist",
                Selector::Album => "album",
                Selector::AlbumArtist => "albumartist",
                Selector::Title => "title",
                Selector::Track => "track",
                Selector::Name => "name",
                Selector::Genre => "genre",
                Selector::Date => "date",
                Selector::OriginalDate => "originaldate",
                Selector::Composer => "composer",
                Selector::Performer => "performer",
                Selector::Conductor => "conductor",
                Selector::Work => "work",
                Selector::Grouping => "grouping",
                Selector::Comment => "comment",
                Selector::Disc => "disc",
                Selector::Label => "label",
                Selector::MusicbrainzAristID => "musicbrainz_aristid",
                Selector::MusicbrainzAlbumID => "musicbrainz_albumid",
                Selector::MusicbrainzAlbumArtistID => "musicbrainz_albumartistid",
                Selector::MusicbrainzTrackID => "musicbrainz_trackid",
                Selector::MusicbrainzReleaseTrackID => "musicbrainz_releasetrackid",
                Selector::MusicbrainzWorkID => "musicbrainz_workid",
                Selector::File => "file",
                Selector::Base => "base",
                Selector::ModifiedSince => "modified-since",
                Selector::AudioFormat => "AudioFormat",
                Selector::Rating => "rating",
                Selector::PlayCount => "playcount",
                Selector::LastPlayed => "lastplayed",
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    UnixEpoch(i64),
    Uint(usize),
}

fn quote_value(x: &Value) -> String {
    match x {
        Value::Text(s) => {
            let mut ret = String::new();

            ret.push('"');
            for c in s.chars() {
                if c == '"' || c == '\\' {
                    ret.push('\\');
                }
                ret.push(c);
            }
            ret.push('"');
            ret
        }
        Value::UnixEpoch(n) => {
            format!("'{}'", n)
        }
        Value::Uint(n) => {
            format!("'{}'", n)
        }
    }
}

#[derive(Clone, Debug)]
pub enum Term {
    UnaryCondition(Selector, Value),
    BinaryCondition(Selector, OpCode, Value),
}

#[derive(Clone, Debug)]
pub enum Conjunction {
    Simple(Box<Expression>, Box<Expression>),
    Compound(Box<Conjunction>, Box<Expression>),
}

#[derive(Clone, Debug)]
pub enum Disjunction {
    Simple(Box<Expression>, Box<Expression>),
    Compound(Box<Disjunction>, Box<Expression>),
}

#[derive(Clone, Debug)]
pub enum Expression {
    Simple(Box<Term>),
    Negation(Box<Expression>),
    Conjunction(Box<Conjunction>),
    Disjunction(Box<Disjunction>),
}

#[cfg(test)]
mod smoke_tests {

    use super::*;
    use crate::filters::*;

    #[test]
    fn test_opcodes() {
        assert!(ExprOpParser::new().parse("==").unwrap() == OpCode::Equality);
        assert!(ExprOpParser::new().parse("!=").unwrap() == OpCode::Inequality);
        assert!(ExprOpParser::new().parse("contains").unwrap() == OpCode::Contains);
        assert!(ExprOpParser::new().parse("=~").unwrap() == OpCode::RegexMatch);
        assert!(ExprOpParser::new().parse("!~").unwrap() == OpCode::RegexExclude);
        assert!(ExprOpParser::new().parse(">").unwrap() == OpCode::GreaterThan);
        assert!(ExprOpParser::new().parse("<").unwrap() == OpCode::LessThan);
        assert!(ExprOpParser::new().parse(">=").unwrap() == OpCode::GreaterThanEqual);
        assert!(ExprOpParser::new().parse("<=").unwrap() == OpCode::LessThanEqual);
    }

    #[test]
    fn test_conditions() {
        assert!(TermParser::new().parse("base 'foo'").is_ok());
        assert!(TermParser::new().parse("artist == 'foo'").is_ok());
        assert!(
            TermParser::new()
                .parse(r#"artist =~ "foo bar \"splat\"!""#)
                .is_ok()
        );
        assert!(TermParser::new().parse("artist =~ 'Pogues'").is_ok());

        match *TermParser::new()
            .parse(r#"base "/Users/me/My Music""#)
            .unwrap()
        {
            Term::UnaryCondition(a, b) => {
                assert!(a == Selector::Base);
                assert!(b == Value::Text(String::from(r#"/Users/me/My Music"#)));
            }
            _ => {
                assert!(false);
            }
        }

        match *TermParser::new()
            .parse(r#"artist =~ "foo bar \"splat\"!""#)
            .unwrap()
        {
            Term::BinaryCondition(t, op, s) => {
                assert!(t == Selector::Artist);
                assert!(op == OpCode::RegexMatch);
                assert!(s == Value::Text(String::from(r#"foo bar "splat"!"#)));
            }
            _ => {
                assert!(false);
            }
        }
    }

    #[test]
    fn test_expressions() {
        assert!(ExpressionParser::new().parse("( base 'foo' )").is_ok());
        assert!(ExpressionParser::new().parse("(base \"foo\")").is_ok());
        assert!(
            ExpressionParser::new()
                .parse("(!(artist == 'value'))")
                .is_ok()
        );
        assert!(
            ExpressionParser::new()
                .parse(r#"((!(artist == "foo bar")) AND (base "/My Music"))"#)
                .is_ok()
        );
    }

    #[test]
    fn test_quoted_expr() {
        eprintln!("test_quoted_expr");
        assert!(
            ExpressionParser::new()
                .parse(r#"(artist =~ "foo\\bar\"")"#)
                .is_ok()
        );
    }

    #[test]
    fn test_real_expression() {
        let result = ExpressionParser::new()
            .parse(r#"(((Artist =~ 'Flogging Molly') OR (artist =~ 'Dropkick Murphys') OR (artist =~ 'Pogues')) AND ((rating > 128) OR (rating == 0)))"#);
        eprintln!("{:#?}", result);
        assert!(result.is_ok());
    }

    #[test]
    fn test_conjunction() {
        assert!(ExpressionParser::new()
            .parse(
                r#"((base "foo") AND (artist == "foo bar") AND (!(file == '/net/mp3/A/a.mp3')))"#
            )
            .is_ok());

        eprintln!("==============================================================================");
        eprintln!("{:#?}", ExpressionParser::new()
            .parse(
                r#"((base 'foo') AND (artist == "foo bar") AND ((!(file == "/net/mp3/A/a.mp3")) OR (file == "/pub/mp3/A/a.mp3")))"#
            ));
        assert!(ExpressionParser::new()
            .parse(
                r#"((base 'foo') AND (artist == "foo bar") AND ((!(file == '/net/mp3/A/a.mp3')) OR (file == '/pub/mp3/A/a.mp3')))"#
            )
            .is_ok());
    }

    #[test]
    fn test_disjunction() {
        assert!(ExpressionParser::new().
                parse(r#"((artist =~ 'Flogging Molly') OR (artist =~ 'Dropkick Murphys') OR (artist =~ 'Pogues'))"#)
                .is_ok());
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                        evaluation logic                                        //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EvalOp {
    And,
    Or,
    Not,
}

impl std::fmt::Display for EvalOp {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            EvalOp::And => write!(f, "And"),
            EvalOp::Or => write!(f, "Or"),
            EvalOp::Not => write!(f, "Not"),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    BadISO8601String {
        text: Vec<u8>,
        back: Backtrace,
    },
    ExpectQuoted {
        text: String,
        back: Backtrace,
    },
    FilterTypeErr {
        text: String,
        back: Backtrace,
    },
    InvalidOperand {
        op: OpCode,
        back: Backtrace,
    },
    OperatorOnStack {
        op: EvalOp,
        back: Backtrace,
    },
    RatingOverflow {
        rating: usize,
        back: Backtrace,
    },
    TooManyOperands {
        num_ops: usize,
        back: Backtrace,
    },
    NumericParse {
        sticker: String,
        source: std::num::ParseIntError,
        back: Backtrace,
    },
    Client {
        source: crate::clients::Error,
        back: Backtrace,
    },
}

impl std::fmt::Display for Error {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::BadISO8601String { text, back: _ } => {
                write!(f, "Bad ISO8601 timestamp: ``{:?}''", text)
            }
            Error::ExpectQuoted { text, back: _ } => write!(f, "Expected quote: ``{}''", text),
            Error::FilterTypeErr { text, back: _ } => {
                write!(f, "Un-expected type in filter ``{}''", text)
            }
            Error::InvalidOperand { op, back: _ } => write!(f, "Invalid operand {}", op),
            Error::OperatorOnStack { op, back: _ } => {
                write!(f, "Operator {} left on parse stack", op)
            }
            Error::RatingOverflow { rating, back: _ } => write!(f, "Rating {} overflows", rating),
            Error::TooManyOperands { num_ops, back: _ } => {
                write!(f, "Too many operands ({})", num_ops)
            }
            Error::NumericParse {
                sticker,
                source,
                back: _,
            } => write!(f, "While parsing sticker {}, got {}", sticker, source),
            Error::Client { source, back: _ } => write!(f, "Client error: {}", source),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            Error::NumericParse {
                sticker: _,
                ref source,
                back: _,
            } => Some(source),
            Error::Client {
                ref source,
                back: _,
            } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn peek(buf: &[u8]) -> Option<char> {
    match buf.len() {
        0 => None,
        _ => Some(buf[0] as char),
    }
}

// advancing a slice by `i` indicies can *not* be this difficult
/// Pop a single byte off of `buf`
fn take1(buf: &mut &[u8], i: usize) -> Result<()> {
    if i > buf.len() {
        return Err(Error::BadISO8601String {
            text: buf.to_vec(),
            back: Backtrace::new(),
        });
    }
    let (_first, second) = buf.split_at(i);
    *buf = second;
    Ok(())
}

/// Pop `i` bytes off of `buf` & parse them as a T
fn take2<T>(buf: &mut &[u8], i: usize) -> Result<T>
where
    T: FromStr,
{
    // 1. check len
    if i > buf.len() {
        return Err(Error::BadISO8601String {
            text: buf.to_vec(),
            back: Backtrace::new(),
        });
    }
    let (first, second) = buf.split_at(i);
    *buf = second;
    // 2. convert to a string
    let s = std::str::from_utf8(first).map_err(|_| Error::BadISO8601String {
        text: buf.to_vec(),
        back: Backtrace::new(),
    })?;
    // 3. parse as a T
    s.parse::<T>().map_err(|_err| Error::BadISO8601String {
        text: buf.to_vec(),
        back: Backtrace::new(),
    }) // Parse*Error => Error
}

/// Parse a timestamp in ISO 8601 format to a chrono DateTime instance
///
/// Surprisingly, I was unable to find an ISO 8601 parser in Rust. I *did* find a crate named
/// iso-8601 that promised to do this, but it seemed derelict & I couldn't see what to do with the
/// parse output in any event. The ISO 8601 format is simple enough that I've chosen to simply
/// hand-parse it.
pub fn parse_iso_8601(mut buf: &mut &[u8]) -> Result<i64> {
    // I wonder if `nom` would be a better choice?

    // The first four characters must be the year (expanded year representation is not supported by
    // this parser).

    let year: i32 = take2(&mut buf, 4)?;

    // Now at this point:
    //   1. we may be done (i.e. buf.len() == 0)
    //   2. we may have the timestamp (peek(buf) => Some('T'))
    //      - day & month := 0, consume the 'T', move on to parsing the time
    //   3. we may have a month in extended format (i.e. peek(buf) => Some('-')
    //      - consume the '-', parse the month & move on to parsing the day
    //   4. we may have a month in basic format (take(buf, 2) => Some('\d\d')
    //      - parse the month & move on to parsing the day
    let mut month = 1;
    let mut day = 1;
    let mut hour = 0;
    let mut minute = 0;
    let mut second = 0;
    if !buf.is_empty() {
        let next = peek(buf);
        if next != Some('T') {
            let mut ext_fmt = false;
            if next == Some('-') {
                take1(buf, 1)?;
                ext_fmt = true;
            }
            month = take2(&mut buf, 2)?;

            // At this point:
            //   1. we may be done (i.e. buf.len() == 0)
            //   2. we may have the timestamp (peek(buf) => Some('T'))
            //   3. we may have the day (in basic or extended format)
            if !buf.is_empty() {
                if peek(buf) != Some('T') {
                    if ext_fmt {
                        take1(&mut buf, 1)?;
                    }
                    day = take2(&mut buf, 2)?;
                }
            }
        }

        // Parse time: at this point, buf will either be empty or begin with 'T'
        if !buf.is_empty() {
            take1(&mut buf, 1)?;
            // If there's a T, there must at least be an hour
            hour = take2(&mut buf, 2)?;
            if !buf.is_empty() {
                let mut ext_fmt = false;
                if peek(buf) == Some(':') {
                    take1(&mut buf, 1)?;
                    ext_fmt = true;
                }
                minute = take2(&mut buf, 2)?;
                if !buf.is_empty() {
                    if ext_fmt {
                        take1(&mut buf, 1)?;
                    }
                    second = take2(&mut buf, 2)?;
                }
            }
        }

        // At this point, there may be a timezone
        if !buf.is_empty() {
            if peek(buf) == Some('Z') {
                return Ok(Utc
                    .with_ymd_and_hms(year, month, day, hour, minute, second)
                    .single()
                    .ok_or(Error::BadISO8601String {
                        text: buf.to_vec(),
                        back: Backtrace::new(),
                    })?
                    .timestamp());
            } else {
                let next = peek(buf);
                if next != Some('-') && next != Some('+') {
                    return Err(Error::BadISO8601String {
                        text: buf.to_vec(),
                        back: Backtrace::new(),
                    });
                }
                let west = next == Some('-');
                take1(&mut buf, 1)?;

                let hours: i32 = take2(&mut buf, 2)?;
                let mut minutes = 0;

                if !buf.is_empty() {
                    if peek(buf) == Some(':') {
                        take1(&mut buf, 1)?;
                    }
                    minutes = take2(&mut buf, 2)?;
                }

                if west {
                    return Ok(FixedOffset::west_opt(hours * 3600 + minutes * 60)
                        .ok_or(Error::BadISO8601String {
                            text: buf.to_vec(),
                            back: Backtrace::new(),
                        })?
                        .with_ymd_and_hms(year, month, day, hour, minute, second)
                        .single()
                        .ok_or(Error::BadISO8601String {
                            text: buf.to_vec(),
                            back: Backtrace::new(),
                        })?
                        .timestamp());
                } else {
                    return Ok(FixedOffset::east_opt(hours * 3600 + minutes * 60)
                        .ok_or(Error::BadISO8601String {
                            text: buf.to_vec(),
                            back: Backtrace::new(),
                        })?
                        .with_ymd_and_hms(year, month, day, hour, minute, second)
                        .single()
                        .ok_or(Error::BadISO8601String {
                            text: buf.to_vec(),
                            back: Backtrace::new(),
                        })?
                        .timestamp());
                }
            }
        }
    }
    Ok(Local
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .ok_or(Error::BadISO8601String {
            text: buf.to_vec(),
            back: Backtrace::new(),
        })?
        .timestamp())
}

#[cfg(test)]
mod iso_8601_tests {

    use super::*;

    #[test]
    fn smoke_tests() {
        let mut b = "19700101T00:00:00Z".as_bytes();
        let t = parse_iso_8601(&mut b).unwrap();
        assert!(t == 0);

        let mut b = "19700101T00:00:01Z".as_bytes();
        let t = parse_iso_8601(&mut b).unwrap();
        assert!(t == 1);

        let mut b = "20210327T02:26:53Z".as_bytes();
        let t = parse_iso_8601(&mut b).unwrap();
        assert_eq!(t, 1616812013);

        let mut b = "20210327T07:29:05-07:00".as_bytes();
        let t = parse_iso_8601(&mut b).unwrap();
        assert_eq!(t, 1616855345);

        let mut b = "2021".as_bytes();
        // Should resolve to midnight, Jan 1 2021 in local time; don't want to test against the
        // timestamp; just make sure it parses
        parse_iso_8601(&mut b).unwrap();
    }
}

/// "Un-quote" a token
///
/// Textual tokens must be quoted, and double-quote & backslashes within backslash-escaped. If the
/// string is quoted with single-quotes, then any single-quotes inside the string will also need
/// to be escaped.
///
/// In fact, *any* characters within may be harmlessly backslash escaped; the MPD implementation
/// walks the the string, skipping backslashes as it goes, so this implementation will do the same.
/// I have named this method in imitation of the corresponding MPD function.
pub fn expect_quoted(qtext: &str) -> Result<String> {
    let mut iter = qtext.chars();
    let quote = iter.next();
    if quote.is_none() {
        return Ok(String::new());
    }

    if quote != Some('\'') && quote != Some('"') {
        return Err(Error::ExpectQuoted {
            text: String::from(qtext),
            back: Backtrace::new(),
        });
    }

    let mut ret = String::new();

    // Walk qtext[1..]; copying characters to `ret'. If a '\' is found, skip to the next character
    // (even if that is a '\'). The last character in qtext should be the closing quote.
    let mut this = iter.next();
    while this != quote {
        if this == Some('\\') {
            this = iter.next();
        }
        match this {
            Some(c) => ret.push(c),
            None => {
                return Err(Error::ExpectQuoted {
                    text: String::from(qtext),
                    back: Backtrace::new(),
                });
            }
        }
        this = iter.next();
    }

    Ok(ret)
}

#[cfg(test)]
mod quoted_tests {

    use super::*;

    #[test]
    fn smoke_tests() {
        let b = r#""foo bar \"splat!\"""#;
        let s = expect_quoted(b).unwrap();
        assert!(s == r#"foo bar "splat!""#);
    }
}

/// Create a closure that will carry out an operator on its argument
///
/// Call this function with an [OpCode] and a value of type `T`. `T` must be [PartialEq],
/// [`PartialOrd`] and [`Copy`]-- an integral type will do. It will return a closure that will carry
/// out the given [OpCode] against the given value. For instance,
/// `make_numeric_closure::<u8>(OpCode::Equality, 11)` will return a closure that takes a `u8` &
/// will return true if its argument is 11 (and false otherwise).
///
/// If [OpCode] is not pertinent to a numeric type, then this function will return Err.
fn make_numeric_closure<'a, T: 'a + PartialEq + PartialOrd + Copy>(
    op: OpCode,
    val: T,
) -> Result<impl Fn(T) -> bool + 'a> {
    // Rust closures each have their own type, so this was the only way I could find to
    // return them from match arms. This seems ugly; perhaps there's something I'm
    // missing.
    //
    // I have no idea why I have to make these `move` closures; T is constrained to by Copy-able,
    // so I would have expected the closure to just take a copy.
    match op {
        OpCode::Equality => Ok(Box::new(move |x: T| x == val) as Box<dyn Fn(T) -> bool>),
        OpCode::Inequality => Ok(Box::new(move |x: T| x != val) as Box<dyn Fn(T) -> bool>),
        OpCode::GreaterThan => Ok(Box::new(move |x: T| x > val) as Box<dyn Fn(T) -> bool>),
        OpCode::LessThan => Ok(Box::new(move |x: T| x < val) as Box<dyn Fn(T) -> bool>),
        OpCode::GreaterThanEqual => Ok(Box::new(move |x: T| x >= val) as Box<dyn Fn(T) -> bool>),
        OpCode::LessThanEqual => Ok(Box::new(move |x: T| x <= val) as Box<dyn Fn(T) -> bool>),
        _ => Err(Error::InvalidOperand {
            op: op,
            back: Backtrace::new(),
        }),
    }
}

async fn eval_numeric_sticker_term<
    // The `FromStr' trait bound is really weird, but if I don't constrain the associated
    // Err type to be `ParseIntError' the compiler complains about not being able to convert
    // it to type `Error'. I'm probably still "thinking in C++" and imagining the compiler
    // instantiating this function for each type (u8, usize, &c) instead of realizing that the Rust
    // compiler is processing this as a first-class function.
    //
    // For instance, I can do the conversion manually, so long as I constrain the Err type
    // to implement std::error::Error. I should probably be doing that, but it clutters the
    // code. I'll figure it out when I need to extend this function to handle non-integral types
    // :)
    T: PartialEq + PartialOrd + Copy + FromStr<Err = std::num::ParseIntError> + std::fmt::Display,
>(
    sticker: &str,
    client: &mut Client,
    op: OpCode,
    numeric_val: T,
    default_val: T,
) -> Result<HashSet<String>> {
    let cmp = make_numeric_closure(op, numeric_val)?;
    // It would be better to idle on the sticker DB & just update our collection on change, but for
    // a first impl. this will do.
    //
    // Call `get_stickers'; this will return a HashMap from song URIs to ratings expressed as text
    // (as all stickers are). This stanza will drain that collection into a new one with the ratings
    // expressed as T.
    //
    // The point is that conversion from text to rating, lastplayed, or whatever can fail; the
    // invocation of `collect' will call `from_iter' to convert a collection of Result-s to a Result
    // of a collection.
    let mut m = client
        .get_stickers(sticker)
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?
        .drain()
        .map(|(k, v)| v.parse::<T>().map(|x| (k, x)))
        .collect::<std::result::Result<HashMap<String, T>, _>>()
        .map_err(|err| Error::NumericParse {
            sticker: String::from(sticker),
            source: err,
            back: Backtrace::new(),
        })?;
    // `m' is now a map of song URI to rating/playcount/wathever (expressed as a T)... for all songs
    // that have the salient sticker.
    //
    // This seems horribly inefficient, but I'm going to fetch all the song URIs in the music DB,
    // and augment `m' with entries of `default_val' for any that are not already there.
    client
        .get_all_songs()
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?
        .drain(..)
        .for_each(|song| {
            if m.get(&song).is_none() {
                m.insert(song, default_val);
            }
        });
    // Now that we don't have to worry about operations that can fail, we can use
    // `filter_map'.
    Ok(m.drain()
        .filter_map(|(k, v)| cmp(v).as_some(k))
        .collect::<HashSet<String>>())
}

/// Convenience struct collecting the names for assorted stickers on which one may search
///
/// While the search terms 'rating', 'playcount' &c are fixed & part of the filter grammar offered
/// by mpdpopm, the precise names of the corresponding stickers are configurable & hence must be
/// passed in. Three references to str is already unweildy IMO, and since I expect the number of
/// stickers on which one can search to grow further, I decided to wrap 'em up in a struct. The
/// lifetime is there to support the caller just using a reference to an existing string rather than
/// making a copy.
pub struct FilterStickerNames<'a> {
    rating: &'a str,
    playcount: &'a str,
    lastplayed: &'a str,
}

impl<'a> FilterStickerNames<'a> {
    pub fn new(rating: &'a str, playcount: &'a str, lastplayed: &'a str) -> FilterStickerNames<'a> {
        FilterStickerNames {
            rating: rating,
            playcount: playcount,
            lastplayed: lastplayed,
        }
    }
}

/// Evaluate a Term
///
/// Take a Term from the Abstract Syntax tree & resolve it to a collection of song URIs. Set `case`
/// to `true` to search case-sensitively & `false` to make the search case-insensitive.
async fn eval_term<'a>(
    term: &Term,
    case: bool,
    client: &mut Client,
    stickers: &FilterStickerNames<'a>,
) -> Result<HashSet<String>> {
    match term {
        Term::UnaryCondition(op, val) => Ok(client
            .find1(&format!("{}", op), &quote_value(&val), case)
            .await
            .map_err(|err| Error::Client {
                source: err,
                back: Backtrace::new(),
            })?
            .drain(..)
            .collect()),
        Term::BinaryCondition(attr, op, val) => {
            if *attr == Selector::Rating {
                match val {
                    Value::Uint(n) => {
                        if *n > 255 {
                            return Err(Error::RatingOverflow {
                                rating: *n,
                                back: Backtrace::new(),
                            });
                        }
                        Ok(eval_numeric_sticker_term(
                            stickers.rating,
                            client,
                            *op,
                            *n as u8,
                            0 as u8,
                        )
                        .await?)
                    }
                    _ => Err(Error::FilterTypeErr {
                        text: format!("filter ratings expect an unsigned int; got {:#?}", val),
                        back: Backtrace::new(),
                    }),
                }
            } else if *attr == Selector::PlayCount {
                match val {
                    Value::Uint(n) => Ok(eval_numeric_sticker_term(
                        stickers.playcount,
                        client,
                        *op,
                        *n,
                        0 as usize,
                    )
                    .await?),
                    _ => Err(Error::FilterTypeErr {
                        text: format!("filter ratings expect an unsigned int; got {:#?}", val),
                        back: Backtrace::new(),
                    }),
                }
            } else if *attr == Selector::LastPlayed {
                match val {
                    Value::UnixEpoch(t) => Ok(eval_numeric_sticker_term(
                        stickers.lastplayed,
                        client,
                        *op,
                        *t,
                        0 as i64,
                    )
                    .await?),
                    _ => Err(Error::FilterTypeErr {
                        text: format!("filter ratings expect an unsigned int; got {:#?}", val),
                        back: Backtrace::new(),
                    }),
                }
            } else {
                Ok(client
                    .find2(
                        &format!("{}", attr),
                        &format!("{}", op),
                        &quote_value(val),
                        case,
                    )
                    .await
                    .map_err(|err| Error::Client {
                        source: err,
                        back: Backtrace::new(),
                    })?
                    .drain(..)
                    .collect())
            }
        }
    }
}

/// The evaluation stack contains logical operators & sets of song URIs
#[derive(Debug)]
enum EvalStackNode {
    Op(EvalOp),
    Result(HashSet<String>),
}

async fn negate_result(
    res: &HashSet<String>,
    client: &mut Client,
) -> std::result::Result<HashSet<String>, Error> {
    Ok(client
        .get_all_songs()
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?
        .drain(..)
        .filter_map(|song| {
            // Some(thing) adds thing, None elides it
            if !res.contains(&song) {
                Some(song)
            } else {
                None
            }
        })
        .collect::<HashSet<String>>())
}

/// Reduce the evaluation stack as far as possible.
///
/// We can pop the stack in two cases:
///
/// 1. S.len() > 2 and S[-3] is either And or Or, and both S[-1] & S[-2] are Result-s
/// 2. S.len() > 1, S[-2] is Not, and S[-1] is a Result
async fn reduce(stack: &mut Vec<EvalStackNode>, client: &mut Client) -> Result<()> {
    loop {
        let mut reduced = false;
        let n = stack.len();
        if n > 1 {
            // Take care to compute the reduction *before* popping the stack-- thank you, borrow
            // checker!
            let reduction = if let (EvalStackNode::Op(EvalOp::Not), EvalStackNode::Result(r)) =
                (&stack[n - 2], &stack[n - 1])
            {
                Some(negate_result(&r, client).await?)
            } else {
                None
            };

            if let Some(res) = reduction {
                stack.pop();
                stack.pop();
                stack.push(EvalStackNode::Result(res));
                reduced = true;
            }
        }
        let n = stack.len();
        if n > 2 {
            // Take care to compute the reduction *before* popping the stack-- thank you, borrow
            // checker!
            let and_reduction = if let (
                EvalStackNode::Op(EvalOp::And),
                EvalStackNode::Result(r1),
                EvalStackNode::Result(r2),
            ) = (&stack[n - 3], &stack[n - 2], &stack[n - 1])
            {
                Some(r1.intersection(&r2).cloned().collect())
            } else {
                None
            };

            if let Some(res) = and_reduction {
                stack.pop();
                stack.pop();
                stack.pop();
                stack.push(EvalStackNode::Result(res));
                reduced = true;
            }
        }
        let n = stack.len();
        if n > 2 {
            let or_reduction = if let (
                EvalStackNode::Op(EvalOp::Or),
                EvalStackNode::Result(r1),
                EvalStackNode::Result(r2),
            ) = (&stack[n - 3], &stack[n - 2], &stack[n - 1])
            {
                Some(r1.union(&r2).cloned().collect())
            } else {
                None
            };

            if let Some(res) = or_reduction {
                stack.pop();
                stack.pop();
                stack.pop();
                stack.push(EvalStackNode::Result(res));
                reduced = true;
            }
        }

        if !reduced {
            break;
        }
    }

    Ok(())
}

/// Evaluate an abstract syntax tree (AST)
pub async fn evaluate<'a>(
    expr: &Expression,
    case: bool,
    client: &mut Client,
    stickers: &FilterStickerNames<'a>,
) -> Result<HashSet<String>> {
    // We maintain *two* stacks, one for parsing & one for evaluation.  Let sp (for "stack(parse)")
    // be a stack of references to nodes in the parse tree.
    let mut sp = Vec::new();
    // Initialize it with the root; as we walk the tree, we'll pop the "most recent" node, and push
    // children.
    sp.push(expr);

    // Let se (for "stack(eval)") be a stack of operators & URIs.
    let mut se = Vec::new();

    // Simple DFS traversal of the AST:
    while !sp.is_empty() {
        // pop the stack...
        let node = sp.pop().unwrap();
        // and dispatch based on what we've got:
        match node {
            // 1. we have a simple term: this can be immediately resolved to a set of song URIs. Do
            // so & push the resulting set onto the evaluation stack.
            Expression::Simple(bt) => se.push(EvalStackNode::Result(
                eval_term(bt, case, client, stickers).await?,
            )),
            // 2. we have a negation: push the "not" operator onto the evaluation stack & the child
            // onto the parse stack.
            Expression::Negation(be) => {
                se.push(EvalStackNode::Op(EvalOp::Not));
                sp.push(&*be);
            }
            // 3. conjunction-- push the "and" operator onto the evaluation stack & the children
            // onto the parse stack (be sure to push the right-hand child first, so it will be
            // popped second)
            // bc is &Box<Conjunction<'a>>, so &**bc is &Conjunction<'a>
            Expression::Conjunction(bc) => {
                let mut conj = &**bc;
                loop {
                    match conj {
                        Conjunction::Simple(bel, ber) => {
                            se.push(EvalStackNode::Op(EvalOp::And));
                            sp.push(&**ber);
                            sp.push(&**bel);
                            break;
                        }
                        Conjunction::Compound(bc, be) => {
                            se.push(EvalStackNode::Op(EvalOp::And));
                            sp.push(&**be);
                            conj = bc;
                        }
                    }
                }
            }
            Expression::Disjunction(bt) => {
                let mut disj = &**bt;
                loop {
                    match disj {
                        Disjunction::Simple(bel, ber) => {
                            se.push(EvalStackNode::Op(EvalOp::Or));
                            sp.push(&ber);
                            sp.push(&bel);
                            break;
                        }
                        Disjunction::Compound(bd, be) => {
                            se.push(EvalStackNode::Op(EvalOp::Or));
                            sp.push(&**be);
                            disj = bd;
                        }
                    }
                }
            }
        }

        reduce(&mut se, client).await?;
    }

    // At this point, sp is empty, but there had better be something on se. Keep reducing the stack
    // until either we can't any further (in which case we error) or there is only one element left
    // (in which case we return that).
    reduce(&mut se, client).await?;

    // Now, se had better have one element, and that element had better be a Result.
    if 1 != se.len() {
        debug!("Too many ({}) operands left on stack:", se.len());
        se.iter()
            .enumerate()
            .for_each(|(i, x)| debug!("    {}: {:#?}", i, x));
        return Err(Error::TooManyOperands {
            num_ops: se.len(),
            back: Backtrace::new(),
        });
    }

    let ret = se.pop().unwrap();
    match ret {
        EvalStackNode::Result(result) => Ok(result),
        EvalStackNode::Op(op) => {
            debug!("Operator left on stack (!?): {:#?}", op);
            Err(Error::OperatorOnStack {
                op: op,
                back: Backtrace::new(),
            })
        }
    }
}

#[cfg(test)]
mod evaluation_tests {

    use super::*;
    use crate::filters::*;

    use crate::clients::Client;
    use crate::clients::test_mock::Mock;

    #[tokio::test]
    async fn smoke() {
        let mock = Box::new(Mock::new(&[(
            r#"find "(base \"foo\")""#,
            "file: foo/a.mp3
Artist: The Foobars
file: foo/b.mp3
Title: b!
OK",
        )]));
        let mut cli = Client::new(mock).unwrap();

        let stickers = FilterStickerNames::new(&"rating", &"playcount", &"lastplayed");

        let expr = ExpressionParser::new().parse(r#"(base "foo")"#).unwrap();
        let result = evaluate(&expr, true, &mut cli, &stickers).await;
        assert!(result.is_ok());

        let g: HashSet<String> = ["foo/a.mp3", "foo/b.mp3"]
            .iter()
            .map(|x| x.to_string())
            .collect();
        assert!(result.unwrap() == g);
    }
}
