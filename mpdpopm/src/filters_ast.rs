// Copyright (C) 2020 Michael Herstine <sp1ff@pobox.com>
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

#[derive(Copy, Clone, Debug)]
pub enum Term<'a> {
    UnaryCondition(&'a str, &'a str),
    BinaryCondition(&'a str, OpCode, &'a str),
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
        assert!(TermParser::new().parse("base foo").is_ok());
        assert!(TermParser::new().parse("artst == foo").is_ok());
        assert!(TermParser::new()
            .parse(r#"artst =~ "foo bar \"splat\"!""#)
            .is_ok());

        match *TermParser::new()
            .parse(r#"base "/Users/me/My Music""#)
            .unwrap()
        {
            Term::UnaryCondition(a, b) => {
                assert!(a == "base");
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
                assert!(t == "artist");
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
        assert!(ExpressionParser::new().parse("( base foo )").is_ok());
        assert!(ExpressionParser::new().parse("(base foo)").is_ok());
        assert!(ExpressionParser::new()
            .parse("(!(artist == value))")
            .is_ok());
        assert!(ExpressionParser::new()
            .parse(r#"((!(Artist == "foo bar")) AND (base "/My Music"))"#)
            .is_ok());
    }

    #[test]
    fn test_conjunction() {
        assert!(ExpressionParser::new()
            .parse(r#"((base foo) AND (Artist == "foo bar") AND (!(file == /net/mp3/A/a.mp3)))"#)
            .is_ok());

        eprintln!("==============================================================================");
        eprintln!("{:#?}", ExpressionParser::new()
            .parse(
                r#"((base foo) AND (Artist == "foo bar") AND ((!(file == /net/mp3/A/a.mp3)) OR (file == /pub/mp3/A/a.mp3)))"#
            ));
        assert!(ExpressionParser::new()
            .parse(
                r#"((base foo) AND (Artist == "foo bar") AND ((!(file == /net/mp3/A/a.mp3)) OR (file == /pub/mp3/A/a.mp3)))"#
            )
            .is_ok());
    }
}
