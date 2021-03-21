// Copyright (C) 2020-2021 Michael Herstine <sp1ff@pobox.com>
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

//! filters-ast -- Types for building the Abstract Syntax Tree when parsing filters

use crate::clients::Client;
use crate::error_from;

use boolinator::Boolinator;
use log::debug;
use snafu::{Backtrace, GenerateBacktrace, Snafu};

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
                Selector::ModifiedSince => "modifiedsince",
                Selector::AudioFormat => "audioformat",
                Selector::Rating => "rating",
                Selector::PlayCount => "playcount",
                Selector::LastPlayed => "lastplayed",
            }
        )
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Term<'a> {
    UnaryCondition(Selector, &'a str),
    BinaryCondition(Selector, OpCode, &'a str),
}

#[derive(Clone, Debug)]
pub enum Conjunction<'a> {
    Simple(Box<Expression<'a>>, Box<Expression<'a>>),
    Compound(Box<Conjunction<'a>>, Box<Expression<'a>>),
}

#[derive(Clone, Debug)]
pub enum Disjunction<'a> {
    Simple(Box<Expression<'a>>, Box<Expression<'a>>),
    Compound(Box<Disjunction<'a>>, Box<Expression<'a>>),
}

#[derive(Clone, Debug)]
pub enum Expression<'a> {
    Simple(Box<Term<'a>>),
    Negation(Box<Expression<'a>>),
    Conjunction(Box<Conjunction<'a>>),
    Disjunction(Box<Disjunction<'a>>),
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
        assert!(TermParser::new()
            .parse(r#"artist =~ "foo bar \"splat\"!""#)
            .is_ok());
        assert!(TermParser::new().parse("artist =~ 'Pogues'").is_ok());

        match *TermParser::new()
            .parse(r#"base "/Users/me/My Music""#)
            .unwrap()
        {
            Term::UnaryCondition(a, b) => {
                assert!(a == Selector::Base);
                assert!(b == r#""/Users/me/My Music""#);
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
                assert!(s == r#""foo bar \"splat\"!""#);
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
        assert!(ExpressionParser::new()
            .parse("(!(artist == 'value'))")
            .is_ok());
        assert!(ExpressionParser::new()
            .parse(r#"((!(artist == "foo bar")) AND (base "/My Music"))"#)
            .is_ok());
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

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Operator {:#?} is not valid in this context.", op))]
    InvalidOperand {
        op: OpCode,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Operator {:#?} left on the stack.", op))]
    OperatorOnStack {
        op: EvalOp,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("{} operands left on the stack; check the logs for details.", num_ops))]
    TooManyOperands {
        num_ops: usize,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

error_from!(crate::clients::Error);
error_from!(std::num::ParseIntError);

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
) -> std::result::Result<impl Fn(T) -> bool + 'a, Error> {
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
            back: Backtrace::generate(),
        }),
    }
}

async fn eval_numeric_sticker_term<
    // The `FromStr' trait bound is really weird, but if I don't constrain the associated
    // Err type to be `ParseIntError' the compiler complains about not being able to convert
    // it to type `Error'. I'm probably still "thinking in C++" and imaginging the compiler
    // instantiating this function for each type (u8, usize, &c) instead of realizing that the Rust
    // compiler is processing this as a first-class function.
    //
    // For instance, I can do the conversion manually, so long as I constrain the Err type
    // to implement std::error::Error. I should probably be doing that, but it clutters the
    // code. I'll figure it out when I need to extend this function to handle non-integral types
    // :)
    T: PartialEq + PartialOrd + Copy + FromStr<Err = std::num::ParseIntError>,
>(
    sticker: &str,
    client: &mut Client,
    op: OpCode,
    val: &str,
    default_val: T,
) -> std::result::Result<HashSet<String>, Error> {
    let numeric_val = val.parse::<T>()?;
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
        .await?
        .drain()
        .map(|(k, v)| v.parse::<T>().map(|x| (k, x)))
        .collect::<Result<HashMap<String, T>, _>>()?;
    // `m' is now a map of song URI to rating/playcount/wathever (expressed as a T)... for all songs
    // that have the salient sticker.
    //
    // This seems horribly inefficient, but I'm going to fetch all the song URIs in the music DB,
    // and augment `m' with entries of `default_val' for any that are not already there.
    client.get_all_songs().await?.drain(..).for_each(|song| {
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
    term: &Term<'a>,
    case: bool,
    client: &mut Client,
    stickers: &FilterStickerNames<'a>,
) -> std::result::Result<HashSet<String>, Error> {
    match term {
        Term::UnaryCondition(op, val) => Ok(client
            .find1(&format!("{}", op), val, case)
            .await?
            .drain(..)
            .collect()),
        Term::BinaryCondition(attr, op, val) => {
            if *attr == Selector::Rating {
                Ok(eval_numeric_sticker_term(stickers.rating, client, *op, val, 0 as u8).await?)
            } else if *attr == Selector::PlayCount {
                Ok(
                    eval_numeric_sticker_term(stickers.playcount, client, *op, val, 0 as usize)
                        .await?,
                )
            } else if *attr == Selector::LastPlayed {
                Ok(
                    eval_numeric_sticker_term(stickers.lastplayed, client, *op, val, 0 as usize)
                        .await?,
                )
            } else {
                Ok(client
                    .find2(&format!("{}", attr), &format!("{}", op), val, case)
                    .await?
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
        .await?
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
async fn reduce(stack: &mut Vec<EvalStackNode>, client: &mut Client) -> Result<(), Error> {
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
    expr: &Expression<'a>,
    case: bool,
    client: &mut Client,
    stickers: &FilterStickerNames<'a>,
) -> std::result::Result<HashSet<String>, Error> {
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
            back: Backtrace::generate(),
        });
    }

    let ret = se.pop().unwrap();
    match ret {
        EvalStackNode::Result(result) => Ok(result),
        EvalStackNode::Op(op) => {
            debug!("Operator left on stack (!?): {:#?}", op);
            Err(Error::OperatorOnStack {
                op: op,
                back: Backtrace::generate(),
            })
        }
    }
}

#[cfg(test)]
mod evaluation_tests {

    use super::*;
    use crate::filters::*;

    use crate::clients::test_mock::Mock;
    use crate::clients::Client;

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
            .into_iter()
            .map(|x| x.to_string())
            .collect();
        assert!(result.unwrap() == g);
    }
}
