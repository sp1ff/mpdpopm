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
//!    - ratings: "rate RATING( TRACK)?"
//!    - set playcount: "setpc PC( TRACK)?"
//!    - set lastplayed: "setlp TIMEESTAMP( TRACK)?"
//!
//! There is no need to provide corresponding accessors since this functionality is already provided
//! via "sticker get". Dedicated accessors could provide the same functionality with slightly more
//! convenience since the sticker name would not have to be specified (as with "sticker get") & may
//! be added at a later date.
//!
//! Additional commands may be added through the [generalized command](commands) feature.

use crate::clients::{Client, IdleClient, PlayerStatus};
use crate::commands::{GeneralizedCommand, PinnedTaggedCmdFuture};
use crate::error_from;
use crate::playcounts::{set_last_played, set_play_count};
use crate::ratings::{set_rating, RatedTrack, RatingRequest};

use boolinator::Boolinator;
use log::debug;
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};

use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("The path `{}' cannot be converted to a UTF-8 string", pth.display()))]
    BadPath { pth: PathBuf },
    #[snafu(display("Invalid unquoted characters in {}", text))]
    InvalidUnquoted { text: String },
    #[snafu(display("Missing closing quotes"))]
    NoClosingQuotes,
    #[snafu(display("No command specified"))]
    NoCommand,
    #[snafu(display("`{}' is not implemented, yet", feature))]
    NotImplemented { feature: String },
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Can't rate the current track when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("Trailing backslash"))]
    TrailingBackslash,
    #[snafu(display(
        "We received messages for an unknown channel `{}'; this is likely a bug; please
consider filing a report to sp1ff@pobox.com",
        chan
    ))]
    UnknownChannel {
        chan: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("We received an unknown message: `{}'", name))]
    UnknownCommand {
        name: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

error_from!(crate::clients::Error);
error_from!(crate::commands::Error);
error_from!(crate::playcounts::Error);
error_from!(crate::ratings::Error);
error_from!(std::num::ParseIntError);
error_from!(std::str::Utf8Error);

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// Break [`text`] up into individual tokens, removing MPD-style quoting
///
/// When a client sends a command to [`mpdpopm`], on the wire it will look like this:
///
/// sendmessage ${CHANNEL} "some-command \"with space\" simple \"'with single' and \\\\\""
///
/// In other words, the MPD "sendmessage" command takes two parameters: the channel and the
/// message. The recipient (us) is responsible for breaking up the message into its constituent
/// parts (a command name & its arguments in our case).
///
/// The message will perforce be quoted according ot the MPD rules:
///
/// 1. an un-quoted token may contain any printable ASCII character except space, tab, ' & "
///
/// 2. to include spaces,. tabs, '-s or "-s, the token must be enclosed in "-s, and any "-s or \-s
///    therein must be backslash escaped
///
/// When the messages is delivered to us, it has already been un-escaped; i.e. we will see:
///
/// some-command "with space" simple "'with single' and \\"
///
/// This function will break that string up into individual tokens with one more level
/// of escaping removed:
///
/// 1. some-command
/// 2. with space
/// 3. simple
/// 4. 'with single' and \
///
/// The [MPD](https://github.com/MusicPlayerDaemon/MPD) has a nice
/// [implementation](https://github.com/MusicPlayerDaemon/MPD/blob/master/src/util/Tokenizer.cxx#L170)
/// that modifies the string in place by copying later characters over top of escape characters,
/// inserting nulls in between the resulting tokens,and then working in terms of pointers to the
/// resulting null-terminated strings.
///
/// I love the signature 'fn tokenize<'a>(code: &'a str) -> impl Iterator<Item=&'a str>' a la "My
/// Favorite Rust Function Signature" <https://www.brandonsmith.ninja/blog/favorite-rust-function>;
/// my first implementation just yielded new String instances, but I think I've figured out how to
/// do it, albeit at the cost of consuming the input String. Since it need to be modified in the
/// process of un-escaping tokens, I don't think that's too bad.
///
/// I'm still not happy with this implementation, but I want to get a working test suite in place
/// before I attempt to refine it.
pub fn tokenize<'a>(buf: &'a mut [u8]) -> impl Iterator<Item = Result<&'a [u8]>> {
    TokenIterator::new(buf)
}

struct TokenIterator<'a> {
    slice: &'a mut [u8],
    input: usize,
}

impl<'a> TokenIterator<'a> {
    pub fn new(slice: &'a mut [u8]) -> TokenIterator {
        TokenIterator {
            slice: slice,
            input: 0,
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
                self.input = inp - out + 1; // +1 to skip the closing"
                while self.input < self.slice.len() && self.slice[self.input] as char == ' ' {
                    self.input += 1;
                }
                Some(Ok(result))
            } else {
                // This is NextUnquoted in MPD; walk self.slice, validating & copying characters
                // until the end or the next whitespace
                let mut i = self.input;
                while i < nslice {
                    if ' ' == self.slice[i] as char {
                        break;
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
                self.input = 0;
                while self.input < self.slice.len() && self.slice[self.input] as char == ' ' {
                    self.input += 1;
                }
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
    pub async fn check_messages<E>(
        &self,
        client: &mut Client,
        idle_client: &mut IdleClient,
        state: PlayerStatus,
        command_chan: &str,
        cmds: &mut E,
    ) -> Result<()>
    where
        E: Extend<PinnedTaggedCmdFuture>,
    {
        let m = idle_client.get_messages().await?;
        for (chan, msgs) in m {
            // Only supporting a single channel, ATM
            (chan == command_chan).as_option().context(UnknownChannel {
                chan: String::from(chan),
            })?;
            for msg in msgs {
                cmds.extend(self.process(msg, client, &state).await?);
            }
        }

        Ok(())
    }

    /// Process a single command
    pub async fn process(
        &self,
        msg: String,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        if msg.starts_with("rate ") {
            self.rate(&msg[5..], client, state).await
        } else if msg.starts_with("setpc ") {
            self.setpc(&msg[6..], client, state).await
        } else if msg.starts_with("setlp ") {
            self.setlp(&msg[6..], client, state).await
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
        let req = RatingRequest::try_from(msg)?;
        let pathb = match req.track {
            RatedTrack::Current => match state {
                PlayerStatus::Stopped => {
                    return Err(Error::PlayerStopped {});
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr.file.clone(),
            },
            RatedTrack::File(p) => p,
            RatedTrack::Relative(_i) => {
                return Err(Error::NotImplemented {
                    feature: String::from("Relative track position"),
                });
            }
        };
        let path: &str = pathb.to_str().context(BadPath { pth: pathb.clone() })?;
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
        .await?)
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
            Some(idx) => (text[..idx].parse::<usize>()?, &text[idx + 1..]),
            None => (text.parse::<usize>()?, ""),
        };
        let file = if track.is_empty() {
            match state {
                PlayerStatus::Stopped => {
                    return Err(Error::PlayerStopped {});
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .context(BadPath {
                        pth: curr.file.clone(),
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
        .await?)
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
            Some(idx) => (text[..idx].parse::<u64>()?, &text[idx + 1..]),
            None => (text.parse::<u64>()?, ""),
        };
        let file = if track.is_empty() {
            match state {
                PlayerStatus::Stopped => {
                    return Err(Error::PlayerStopped {});
                }
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .context(BadPath {
                        pth: curr.file.clone(),
                    })?
                    .to_string(),
            }
        } else {
            track.to_string()
        };
        set_last_played(client, self.lastplayed_sticker, &file, lp).await?;
        Ok(None)
    }

    /// Handle generalized commands
    async fn maybe_handle_generalized_command(
        &self,
        msg: String,
        state: &PlayerStatus,
    ) -> Result<Option<PinnedTaggedCmdFuture>> {
        let mut buf = msg.into_bytes();
        let mut args: VecDeque<&str> = tokenize(&mut buf)
            .map(|r| match r {
                Ok(buf) => Ok(std::str::from_utf8(buf)?),
                Err(err) => Err(err),
            })
            .collect::<Result<VecDeque<&str>>>()?;

        let cmd = match args.pop_front() {
            Some(x) => x,
            None => {
                return Err(Error::NoCommand);
            }
        };
        let gen_cmd = self
            .gen_cmds
            .get(cmd)
            .context(UnknownCommand { name: cmd.clone() })?;
        Ok(Some(gen_cmd.execute(args.iter().cloned(), &state)?))
    }
}
