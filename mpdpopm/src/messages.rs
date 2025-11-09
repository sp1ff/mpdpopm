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

//! # messages
//!
//! Process incoming messages to the [mpdpopm](https://github.com/sp1ff/mpdpopm) daemon.
//!
//! # Introduction
//!
//! The [mpdpopm](https://github.com/sp1ff/mpdpopm) daemon accepts commands over a dedicated
//! [channel](https://www.musicpd.org/doc/html/protocol.html#client-to-client). It also provides for
//! a generalized framework in which the [mpdpopm](https://github.com/sp1ff/mpdpopm) administrator
//! can define new commands backed by arbitrary command execution server-side.
//!
//! # Commands
//!
//! The following commands are built-in:
//!
//!    - set rating: `rate RATING( TRACK)?`
//!    - set playcount: `setpc PC( TRACK)?`
//!    - set lastplayed: `setlp TIMESTAMP( TRACK)?`
//!
//! There is no need to provide corresponding accessors since this functionality is already provided
//! via "sticker get". Dedicated accessors could provide the same functionality with slightly more
//! convenience since the sticker name would not have to be specified (as with "sticker get") & may
//! be added at a later date.
//!
//! I'm expanding the MPD filter functionality to include attributes tracked by mpdpopm:
//!
//!    - findadd replacement: `findadd FILTER [sort TYPE] [window START:END]`
//!      (cf. [here](https://www.musicpd.org/doc/html/protocol.html#the-music-database))
//!
//!    - searchadd replacement: `searchadd FILTER [sort TYPE] [window START:END]`
//!      (cf. [here](https://www.musicpd.org/doc/html/protocol.html#the-music-database))
//!
//! Additional commands may be added through the
//! [generalized commands](crate::commands#the-generalized-command-framework) feature.

use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::path::PathBuf;

use snafu::{prelude::*, Backtrace, IntoError};
use tracing::debug;

use crate::{
    clients::{Client, IdleClient, PlayerStatus},
    commands::{GeneralizedCommand, PinnedTaggedCmdFuture},
    filters::ExpressionParser,
    filters_ast::{evaluate, FilterStickerNames},
    playcounts::{set_last_played, set_play_count},
    ratings::{set_rating, RatedTrack, RatingRequest},
};

////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Bad path: {:?}", pth))]
    BadPath { pth: PathBuf },
    #[snafu(display("Parse error: ``{}''", msg))]
    FilterParseError { msg: String },
    #[snafu(display("Invalid unquoted character {}", c))]
    InvalidChar { c: u8 },
    #[snafu(display("Missing closing quotes"))]
    NoClosingQuotes,
    #[snafu(display("No command specified"))]
    NoCommand,
    #[snafu(display("`{}' not implemented, yet", feature))]
    NotImplemented { feature: String },
    #[snafu(display("Can't operate on the current track when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("Trailing backslash"))]
    TrailingBackslash,
    #[snafu(display("We received messages for an unknown channel `{}`; this is likely a bug; please consider filing a report to sp1ff@pobox.com", chan))]
    UnknownChannel { chan: String, backtrace: Backtrace },
    #[snafu(display("We received an unknown message ``{}''", name))]
    UnknownCommand { name: String, backtrace: Backtrace },
    #[snafu(display("Client error: {}", source))]
    Client {
        source: crate::clients::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("Ratings eror: {}", source))]
    Ratings {
        source: crate::ratings::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("Playcount error: {}", source))]
    Playcount {
        source: crate::playcounts::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("Filter error: {}", source))]
    Filter {
        source: crate::filters_ast::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("Command error: {}", source))]
    Command {
        source: crate::commands::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("UTF8 error {} ({:#?})", source, buf))]
    Utf8 {
        source: std::str::Utf8Error,
        buf: Vec<u8>,
        backtrace: Backtrace,
    },
    #[snafu(display("``{}''L {}", source, text))]
    ExpectedInt {
        source: std::num::ParseIntError,
        text: String,
        backtrace: Backtrace,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// Break `buf` up into individual tokens while removing MPD-style quoting.
///
/// When a client sends a command to [mpdpopm](crate), it will look like this on the wire:
///
/// ```text
/// sendmessage ${CHANNEL} "some-command \"with space\" simple \"'with single' and \\\\\""
/// ```
///
/// In other words, the MPD "sendmessage" command takes two parameters: the channel and the
/// message. The recipient (i.e. us) is responsible for breaking up the message into its constituent
/// parts (a command name & its arguments in our case).
///
/// The message will perforce be quoted according ot the MPD rules:
///
/// 1. an un-quoted token may contain any printable ASCII character except space, tab, ' & "
///
/// 2. to include spaces, tabs, '-s or "-s, the token must be enclosed in "-s, and any "-s or \\-s
///    therein must be backslash escaped
///
/// When the messages is delivered to us, it has already been un-escaped; i.e. we will see the
/// string:
///
/// ```text
/// some-command "with space" simple "'with single' and \\"
/// ```
///
/// This function will break that string up into individual tokens with one more level
/// of escaping removed; i.e. it will return an iterator that will yield the four tokens:
///
/// 1. some-command
/// 2. with space
/// 3. simple
/// 4. 'with single' and \\
///
/// [MPD](https://github.com/MusicPlayerDaemon/MPD) has a nice
/// [implementation](https://github.com/MusicPlayerDaemon/MPD/blob/master/src/util/Tokenizer.cxx#L170)
/// that modifies the string in place by copying subsequent characters on top of escape characters
/// in the same buffer, inserting nulls in between the resulting tokens,and then working in terms of
/// pointers to the resulting null-terminated strings.
///
/// Once I realized that I could split slices I saw how to implement an Iterator that do the same
/// thing (an idiomatic interface to the tokenization backed by a zero-copy implementation). I was
/// inspired by [My Favorite Rust Function
/// Signature](<https://www.brandonsmith.ninja/blog/favorite-rust-function>).
///
/// NB. This method works in terms of a slice of [`u8`] because we can't index into Strings in
/// Rust, and MPD deals only in terms of ASCII at any rate.
pub fn tokenize<'a>(buf: &'a mut [u8]) -> impl Iterator<Item = Result<&'a [u8]>> {
    TokenIterator::new(buf)
}

struct TokenIterator<'a> {
    /// The slice on which we operate; modified in-place as we yield tokens
    slice: &'a mut [u8],
    /// Index into [`slice`] of the first non-whitespace character
    input: usize,
}

impl<'a> TokenIterator<'a> {
    pub fn new(slice: &'a mut [u8]) -> TokenIterator<'a> {
        let input = match slice.iter().position(|&x| x > 0x20) {
            Some(n) => n,
            None => slice.len(),
        };
        TokenIterator {
            slice: slice,
            input: input,
        }
    }
}

impl<'a> Iterator for TokenIterator<'a> {
    type Item = Result<&'a [u8]>;

    fn next<'s>(&'s mut self) -> Option<Self::Item> {
        let nslice = self.slice.len();
        if self.slice.is_empty() || self.input == nslice {
            None
        } else {
            if '"' == self.slice[self.input] as char {
                // This is NextString in MPD: walk self.slice, un-escaping characters, until we find
                // a closing ". Note that we un-escape by moving characters forward in the slice.
                let mut inp = self.input + 1;
                let mut out = self.input;
                while self.slice[inp] as char != '"' {
                    if '\\' == self.slice[inp] as char {
                        inp += 1;
                        if inp == nslice {
                            return Some(Err(Error::TrailingBackslash));
                        }
                    }
                    self.slice[out] = self.slice[inp];
                    out += 1;
                    inp += 1;
                    if inp == nslice {
                        return Some(Err(Error::NoClosingQuotes));
                    }
                }
                // The next token is in self.slice[self.input..out] and self.slice[inp] is "
                let tmp = std::mem::replace(&mut self.slice, &mut []);
                let (_, tmp) = tmp.split_at_mut(self.input);
                let (result, new_slice) = tmp.split_at_mut(out - self.input);
                self.slice = new_slice;
                // strip any leading whitespace
                self.input = inp - out + 1; // +1 to skip the closing "
                while self.input < self.slice.len() && self.slice[self.input] as char == ' ' {
                    self.input += 1;
                }
                Some(Ok(result))
            } else {
                // This is NextUnquoted in MPD; walk self.slice, validating characters until the end
                // or the next whitespace
                let mut i = self.input;
                while i < nslice {
                    if 0x20 >= self.slice[i] {
                        break;
                    }
                    if self.slice[i] as char == '"' || self.slice[i] as char == '\'' {
                        return Some(Err(Error::InvalidChar { c: self.slice[i] }));
                    }
                    i += 1;
                }
                // The next token is in self.slice[self.input..i] & self.slice[i] is either one-
                // past-the end or whitespace.
                let tmp = std::mem::replace(&mut self.slice, &mut []);
                let (_, tmp) = tmp.split_at_mut(self.input);
                let (result, new_slice) = tmp.split_at_mut(i - self.input);
                self.slice = new_slice;
                // strip any leading whitespace
                self.input = match self.slice.iter().position(|&x| x > 0x20) {
                    Some(n) => n,
                    None => self.slice.len(),
                };
                Some(Ok(result))
            }
        }
    }
}

#[cfg(test)]
mod tokenize_tests {

    use super::tokenize;
    use super::Result;

    #[test]
    fn tokenize_smoke() {
        let mut buf1 = String::from("some-command").into_bytes();
        let x1: Vec<&[u8]> = tokenize(&mut buf1).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x1[0], b"some-command");

        let mut buf2 = String::from("a b").into_bytes();
        let x2: Vec<&[u8]> = tokenize(&mut buf2).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x2[0], b"a");
        assert_eq!(x2[1], b"b");

        let mut buf3 = String::from("a \"b c\"").into_bytes();
        let x3: Vec<&[u8]> = tokenize(&mut buf3).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x3[0], b"a");
        assert_eq!(x3[1], b"b c");

        let mut buf4 = String::from("a \"b c\" d").into_bytes();
        let x4: Vec<&[u8]> = tokenize(&mut buf4).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x4[0], b"a");
        assert_eq!(x4[1], b"b c");
        assert_eq!(x4[2], b"d");

        let mut buf5 = String::from("simple-command \"with space\" \"with '\"").into_bytes();
        let x5: Vec<&[u8]> = tokenize(&mut buf5).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x5[0], b"simple-command");
        assert_eq!(x5[1], b"with space");
        assert_eq!(x5[2], b"with '");

        let mut buf6 = String::from("cmd \"with\\\\slash and space\"").into_bytes();
        let x6: Vec<&[u8]> = tokenize(&mut buf6).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x6[0], b"cmd");
        assert_eq!(x6[1], b"with\\slash and space");

        let mut buf7 = String::from(" cmd  \"with\\\\slash and space\"	").into_bytes();
        let x7: Vec<&[u8]> = tokenize(&mut buf7).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(x7[0], b"cmd");
        assert_eq!(x7[1], b"with\\slash and space");
    }

    #[test]
    fn tokenize_filter() {
        let mut buf1 = String::from(r#""(artist =~ \"foo\\\\bar\\\"\")""#).into_bytes();
        let x1: Vec<&[u8]> = tokenize(&mut buf1).collect::<Result<Vec<&[u8]>>>().unwrap();
        assert_eq!(1, x1.len());
        eprintln!("x1[0] is ``{}''", std::str::from_utf8(x1[0]).unwrap());
        assert_eq!(
            std::str::from_utf8(x1[0]).unwrap(),
            r#"(artist =~ "foo\\bar\"")"#
        );
    }
}

/// Collective state needed for processing messages, both built-in & generalized
pub struct MessageProcessor<'a, I1, I2>
where
    I1: Iterator<Item = String> + Clone,
    I2: Iterator<Item = String> + Clone,
{
    music_dir: &'a str,
    rating_sticker: &'a str,
    ratings_cmd: &'a str,
    ratings_cmd_args: I1,
    playcount_sticker: &'a str,
    playcount_cmd: &'a str,
    playcount_cmd_args: I2,
    lastplayed_sticker: &'a str,
    gen_cmds: HashMap<String, GeneralizedCommand>,
}

impl<I1, I2> MessageProcessor<'_, I1, I2>
where
    I1: Iterator<Item = String> + Clone,
    I2: Iterator<Item = String> + Clone,
{
    /// Whip up a new instance; other than cloning the iterators, should just hold references in the
    /// enclosing scope
    pub fn new<'a, IGC>(
        music_dir: &'a str,
        rating_sticker: &'a str,
        ratings_cmd: &'a str,
        ratings_cmd_args: I1,
        playcount_sticker: &'a str,
        playcount_cmd: &'a str,
        playcount_cmd_args: I2,
        lastplayed_sticker: &'a str,
        gen_cmds: IGC,
    ) -> MessageProcessor<'a, I1, I2>
    where
        IGC: Iterator<Item = (String, GeneralizedCommand)>,
    {
        MessageProcessor {
            music_dir: music_dir,
            rating_sticker: rating_sticker,
            ratings_cmd: ratings_cmd,
            ratings_cmd_args: ratings_cmd_args.clone(),
            playcount_sticker: playcount_sticker,
            playcount_cmd: playcount_cmd,
            playcount_cmd_args: playcount_cmd_args.clone(),
            lastplayed_sticker: lastplayed_sticker,
            gen_cmds: HashMap::from_iter(gen_cmds),
        }
    }

    /// Read messages off the commands channel & dispatch 'em
    pub async fn check_messages<'a, E>(
        &self,
        client: &mut Client,
        idle_client: &mut IdleClient,
        state: PlayerStatus,
        command_chan: &str,
        cmds: &mut E,
        stickers: &FilterStickerNames<'a>,
    ) -> Result<()>
    where
        E: Extend<PinnedTaggedCmdFuture>,
    {
        let m = idle_client.get_messages().await.context(ClientSnafu)?;
        for (chan, msgs) in m {
            // Only supporting a single channel, ATM
            <bool as boolinator::Boolinator>::ok_or_else(chan == command_chan, || {
                UnknownChannelSnafu {
                    chan: String::from(chan),
                }
                .build()
            })?;
            for msg in msgs {
                cmds.extend(self.process(msg, client, &state, stickers).await?);
            }
        }

        Ok(())
    }

    /// Process a single command
    pub async fn process<'a>(
        &self,
        msg: String,
        client: &mut Client,
        state: &PlayerStatus,
        stickers: &FilterStickerNames<'a>,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        if msg.starts_with("rate ") {
            self.rate(&msg[5..], client, state).await
        } else if msg.starts_with("setpc ") {
            self.setpc(&msg[6..], client, state).await
        } else if msg.starts_with("setlp ") {
            self.setlp(&msg[6..], client, state).await
        } else if msg.starts_with("findadd ") {
            self.findadd(msg[8..].to_string(), client, stickers, state)
                .await
        } else if msg.starts_with("searchadd ") {
            self.searchadd(msg[10..].to_string(), client, stickers, state)
                .await
        } else {
            self.maybe_handle_generalized_command(msg, state).await
        }
    }

    /// Handle rating message: "RATING( TRACK)?"
    async fn rate(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        let req = RatingRequest::try_from(msg).context(RatingsSnafu)?;
        let pathb = match req.track {
            RatedTrack::Current => match state {
                PlayerStatus::Stopped => {
                    return PlayerStoppedSnafu.fail();
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr.file.clone(),
            },
            RatedTrack::File(p) => p,
            RatedTrack::Relative(_i) => {
                return NotImplementedSnafu {
                    feature: String::from("Relative track position"),
                }
                .fail();
            }
        };
        let path: &str = pathb
            .to_str()
            .ok_or_else(|| BadPathSnafu { pth: pathb.clone() }.build())?;
        debug!("Setting a rating of {} for `{}'.", req.rating, path);

        Ok(set_rating(
            client,
            self.rating_sticker,
            path,
            req.rating,
            self.ratings_cmd,
            self.ratings_cmd_args.clone(),
            self.music_dir,
        )
        .await
        .context(RatingsSnafu)?)
    }

    /// Handle `setpc': "PC( TRACK)?"
    async fn setpc(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        let text = msg.trim();
        let (pc, track) = match text.find(char::is_whitespace) {
            Some(idx) => (
                text[..idx].parse::<usize>().context(ExpectedIntSnafu {
                    text: String::from(text),
                })?,
                &text[idx + 1..],
            ),
            None => (
                text.parse::<usize>().context(ExpectedIntSnafu {
                    text: String::from(text),
                })?,
                "",
            ),
        };
        let file = if track.is_empty() {
            match state {
                PlayerStatus::Stopped => {
                    return PlayerStoppedSnafu.fail();
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .ok_or_else(|| {
                        BadPathSnafu {
                            pth: curr.file.clone(),
                        }
                        .build()
                    })?
                    .to_string(),
            }
        } else {
            track.to_string()
        };
        if self.playcount_cmd.is_empty() {
            return Ok(None);
        }

        Ok(set_play_count(
            client,
            self.playcount_sticker,
            &file,
            pc,
            self.playcount_cmd,
            &mut self.playcount_cmd_args.clone(),
            self.music_dir,
        )
        .await
        .context(PlaycountSnafu)?)
    }

    /// Handle `setlp': "LASTPLAYED( TRACK)?"
    async fn setlp(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        let text = msg.trim();
        let (lp, track) = match text.find(char::is_whitespace) {
            Some(idx) => (
                text[..idx].parse::<u64>().context(ExpectedIntSnafu {
                    text: String::from(text),
                })?,
                &text[idx + 1..],
            ),
            None => (
                text.parse::<u64>().context(ExpectedIntSnafu {
                    text: String::from(text),
                })?,
                "",
            ),
        };
        let file = if track.is_empty() {
            match state {
                PlayerStatus::Stopped => {
                    return PlayerStoppedSnafu.fail();
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .ok_or_else(|| {
                        BadPathSnafu {
                            pth: curr.file.clone(),
                        }
                        .build()
                    })?
                    .to_string(),
            }
        } else {
            track.to_string()
        };
        set_last_played(client, self.lastplayed_sticker, &file, lp)
            .await
            .context(PlaycountSnafu)?;
        Ok(None)
    }

    async fn findadd<'a>(
        &self,
        msg: String,
        client: &mut Client,
        stickers: &FilterStickerNames<'a>,
        _state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        // Tokenize the message
        let mut buf = msg.into_bytes();
        let args: VecDeque<&str> = tokenize(&mut buf)
            .map(|r| match r {
                Ok(buf) => Ok(std::str::from_utf8(buf).context(Utf8Snafu { buf: buf.to_vec() })?),
                Err(err) => Err(err),
            })
            .collect::<Result<VecDeque<&str>>>()?;

        debug!("findadd arguments: {:#?}", args);

        // there should be 1, 3 or 5 arguments. `sort' & `window' are not supported, yet.

        // ExpressionParser's not terribly ergonomic: it returns a ParesError<L, T, E>; T is the
        // offending token, which has the same lifetime as our input, which makes it tough to
        // capture.  Nor is there a convenient way in which to treat all variants other than the
        // Error Trait.
        let ast = match ExpressionParser::new().parse(args[0]) {
            Ok(ast) => ast,
            Err(err) => {
                return FilterParseSnafu {
                    msg: format!("{}", err),
                }
                .fail();
            }
        };

        debug!("ast: {:#?}", ast);

        let mut results = Vec::new();
        for song in evaluate(&ast, true, client, stickers)
            .await
            .context(FilterSnafu)?
        {
            results.push(client.add(&song).await);
        }
        match results
            .into_iter()
            .collect::<std::result::Result<Vec<()>, crate::clients::Error>>()
        {
            Ok(_) => Ok(None),
            Err(err) => Err(ClientSnafu.into_error(err)),
        }
    }

    async fn searchadd<'a>(
        &self,
        msg: String,
        client: &mut Client,
        stickers: &FilterStickerNames<'a>,
        _state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        // Tokenize the message
        let mut buf = msg.into_bytes();
        let args: VecDeque<&str> = tokenize(&mut buf)
            .map(|r| match r {
                Ok(buf) => Ok(std::str::from_utf8(buf).context(Utf8Snafu { buf: buf.to_vec() })?),
                Err(err) => Err(err),
            })
            .collect::<Result<VecDeque<&str>>>()?;

        debug!("searchadd arguments: {:#?}", args);

        // there should be 1, 3 or 5 arguments. `sort' & `window' are not supported, yet.

        // ExpressionParser's not terribly ergonomic: it returns a ParesError<L, T, E>; T is the
        // offending token, which has the same lifetime as our input, which makes it tough to
        // capture.  Nor is there a convenient way in which to treat all variants other than the
        // Error Trait.
        let ast = match ExpressionParser::new().parse(args[0]) {
            Ok(ast) => ast,
            Err(err) => {
                return FilterParseSnafu {
                    msg: format!("{}", err),
                }
                .fail();
            }
        };

        debug!("ast: {:#?}", ast);

        let mut results = Vec::new();
        for song in evaluate(&ast, false, client, stickers)
            .await
            .context(FilterSnafu)?
        {
            results.push(client.add(&song).await);
        }
        match results
            .into_iter()
            .collect::<std::result::Result<Vec<()>, crate::clients::Error>>()
        {
            Ok(_) => Ok(None),
            Err(err) => Err(ClientSnafu.into_error(err)),
        }
    }

    async fn maybe_handle_generalized_command(
        &self,
        msg: String,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        let mut buf = msg.into_bytes();
        let mut args: VecDeque<&str> = tokenize(&mut buf)
            .map(|r| match r {
                Ok(buf) => Ok(std::str::from_utf8(buf).context(Utf8Snafu { buf: buf.to_vec() })?),
                Err(err) => Err(err),
            })
            .collect::<Result<VecDeque<&str>>>()?;

        let cmd = match args.pop_front() {
            Some(x) => x,
            None => {
                return NoCommandSnafu {}.fail();
            }
        };
        let gen_cmd = self.gen_cmds.get(cmd).ok_or_else(|| {
            UnknownCommandSnafu {
                name: String::from(cmd),
            }
            .build()
        })?;
        Ok(Some(
            gen_cmd
                .execute(args.iter().cloned(), &state)
                .context(CommandSnafu)?,
        ))
    }
}
