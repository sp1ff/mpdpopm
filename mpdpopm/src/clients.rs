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

//! mpd clients and associated utilities.
//!
//! # Introduction
//!
//! This module contains basic types implementing various `mpd' client operations. Cf. the [mpd
//! protocol](http://www.musicpd.org/doc/protocol/). Since issuing the "idle" command will tie up
//! the connection, mpd clients often use multiple connections to the server (one to listen for
//! updates, one or more on which to issue commands). This modules provides two different client
//! types: [`Client`] for general-purpose use and [`IdleClient`] for long-lived connections
//! listening for server notifiations.
//!
//! There *is* another idiom (used in libmpdel, e.g.): open a single connection & issue an "idle"
//! command. When you want to issue a command, send a "noidle", then the command, then "idle" again.
//! This isn't a race condition, as the server will buffer any changes that took place when you
//! were not idle & send them when you re-issue the "idle" command.

use crate::error_from;

use async_trait::async_trait;
use boolinator::Boolinator;
use log::{debug, info};
use regex::Regex;
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};
use tokio::{
    net::{TcpStream, ToSocketAddrs, UnixStream},
    prelude::*,
};

use std::{
    collections::HashMap,
    convert::TryFrom,
    fmt,
    marker::{Send, Unpin},
    path::{Path, PathBuf},
    str::FromStr,
};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                               Error                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An enumeration of `mpd' client errors
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error upon connecting to the mpd server; includes the text returned (if any)
    #[snafu(display("Failed to connect to the mpd server: `{}'", text))]
    Connect {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "readmessages"
    #[snafu(display("Failed to read messages: `{}'", text))]
    GetMessages {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "sticker set"
    #[snafu(display("Failed to set sticker: `{}'", text))]
    StickerSet {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "sticker get"
    #[snafu(display("Failed to get sticker: `{}'", text))]
    StickerGet {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "sendmessage"
    #[snafu(display("Failed to send message: `{}'", text))]
    SendMessage {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "subscribe"
    #[snafu(display("Failed to subscribe to our subsystems of interest: `{}'", text))]
    Subscribe {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "idle"
    #[snafu(display("Failed to idle: `{}'", text))]
    Idle {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Unknown idle subsystem
    #[snafu(display(
        "Uknown subsystem `{}'returned in respones to the `idle' command. This is \
likely a bug; please consider reporting it to sp1ff@pobox.com",
        text
    ))]
    IdleSubsystem {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Couldn't parse PlayState
    #[snafu(display("Couldn't parse play state from {}", text))]
    PlayState {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Couldn't parse SongId
    #[snafu(display("Couldn't parse songid from {}", text))]
    SongId {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Couldn't parse Elapsed
    #[snafu(display("Couldn't parse elapsed from {}", text))]
    Elapsed {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Couldn't parse song file
    #[snafu(display("Couldn't parse song file from {}", text))]
    SongFile {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Couldn't parse Duration
    #[snafu(display("Couldn't parse elapsed from {}", text))]
    Duration {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Unknown player state
    #[snafu(display("Unknown player state {}", state))]
    UnknownState {
        state: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "update"
    #[snafu(display("Failed to update DB: `{}'", text))]
    UpdateDB {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in respone to "listplaylists"
    #[snafu(display("Failed to list stored playlists: `{}'", text))]
    GetStoredPlaylists {
        text: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Error in converting a sticker to the desired type
    #[snafu(display("Failed to convert sticker {}: {}", sticker, error))]
    BadStickerConversion {
        sticker: String,
        error: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

error_from!(crate::commands::Error);
error_from!(regex::Error);
error_from!(std::io::Error);
error_from!(std::num::ParseFloatError);
error_from!(std::num::ParseIntError);
error_from!(std::string::FromUtf8Error);

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// A description of the current track, suitable for our purposes (as in, it only tracks the
/// attributes needed for this module's functionality).
#[derive(Clone, Debug)]
pub struct CurrentSong {
    /// Identifier, unique within the play queue, identifying this particular track; if the same
    /// file is listed twice in the `mpd' play queue each instance will get a distinct songid
    pub songid: u64,
    /// Path, relative to `mpd' music directory root of this track
    pub file: std::path::PathBuf,
    /// Elapsed time, in seconds, in this track
    pub elapsed: f64,
    /// Total track duration, in seconds
    pub duration: f64,
}

impl CurrentSong {
    fn new(songid: u64, file: std::path::PathBuf, elapsed: f64, duration: f64) -> CurrentSong {
        CurrentSong {
            songid: songid,
            file: file,
            elapsed: elapsed,
            duration: duration,
        }
    }
    /// Compute the ratio of the track that has elapsed, expressed as a floating point between 0 & 1
    pub fn played_pct(&self) -> f64 {
        self.elapsed / self.duration
    }
}

#[derive(Clone, Debug)]
pub enum PlayerStatus {
    Play(CurrentSong),
    Pause(CurrentSong),
    Stopped,
}

impl PlayerStatus {
    pub fn current_song(&self) -> Option<&CurrentSong> {
        match self {
            PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => Some(curr),
            PlayerStatus::Stopped => None,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           Connection                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Represents a simple, textual request/response protocol like that employed by [`mpd`].
///
/// This trait also enables unit testing client implementations. Note that it is async.
///
/// [`mpd`]: https://www.musicpd.org/doc/html/protocol.html
#[async_trait]
pub trait RequestResponse {
    async fn req(&mut self, msg: &str) -> Result<String>;
    /// The hint is used to size the buffer prior to reading the response
    async fn req_w_hint(&mut self, msg: &str, hint: usize) -> Result<String>;
}

#[cfg(test)]
pub mod test_mock {

    use super::*;

    /// Mock is an implementation of [`RequestRespone`] that checks expected requests & responses,
    /// and will panic if it sees anything unexpected
    pub struct Mock {
        inmsgs: Vec<String>,
        outmsgs: Vec<String>,
    }

    impl Mock {
        pub fn new(convo: &[(&str, &str)]) -> Mock {
            let (left, right): (Vec<&str>, Vec<&str>) = convo.iter().copied().rev().unzip();
            Mock {
                inmsgs: left.iter().map(|x| x.to_string()).collect(),
                outmsgs: right.iter().map(|x| x.to_string()).collect(),
            }
        }
    }

    #[async_trait]
    impl RequestResponse for Mock {
        async fn req(&mut self, msg: &str) -> Result<String> {
            self.req_w_hint(msg, 512).await
        }
        async fn req_w_hint(&mut self, msg: &str, _hint: usize) -> Result<String> {
            assert_eq!(msg, self.inmsgs.pop().unwrap());
            Ok(self.outmsgs.pop().unwrap())
        }
    }

    #[tokio::test]
    async fn mock_smoke_test() {
        let mut mock = Mock::new(&[("ping", "pong"), ("from", "to")]);
        assert_eq!(mock.req("ping").await.unwrap(), "pong");
        assert_eq!(mock.req("from").await.unwrap(), "to");
    }

    #[tokio::test]
    #[should_panic]
    async fn mock_negative_test() {
        let mut mock = Mock::new(&[("ping", "pong")]);
        assert_eq!(mock.req("ping").await.unwrap(), "pong");
        let _should_panic = mock.req("not there!").await.unwrap();
    }
}

/// mpd connections talk one protocol over either a TCP or a Unix socket
pub struct MpdConnection<T: AsyncRead + AsyncWrite + Send + Unpin> {
    sock: T,
    _proto_ver: String,
}

/// MpdConnection implements RequestResponse using the usual (async) socket I/O
///
/// The callers need not include the trailing newline in their requests; the implementation will
/// append it.
#[async_trait]
impl<T> RequestResponse for MpdConnection<T>
where
    T: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn req(&mut self, msg: &str) -> Result<String> {
        self.req_w_hint(msg, 512).await
    }
    async fn req_w_hint(&mut self, msg: &str, hint: usize) -> Result<String> {
        self.sock.write_all(format!("{}\n", msg).as_bytes()).await?;
        let mut buf = Vec::with_capacity(hint);

        // Given the request/response nature of the MPD protocol, our callers expect a complete
        // response. Therefore we need to loop here until we see either "...^OK\n" or
        // "...^ACK...\n".
        let mut cb = 0; // # bytes read so far
        let mut more = true; // true as long as there is more to read
        while more {
            cb += self.sock.read_buf(&mut buf).await?;

            // The shortest complete response has three bytes. If the final byte in `buf' is not a
            // newline, then don't bother looking further.
            if cb > 2 && char::from(buf[cb - 1]) == '\n' {
                // If we're here, `buf' *may* contain a complete response. Search backward for the
                // previous newline. It may not exist: many responses are of the form "OK\n".
                let mut idx = cb - 2;
                while idx > 0 {
                    if char::from(buf[idx]) == '\n' {
                        idx += 1;
                        break;
                    }
                    idx -= 1;
                }

                if (idx + 2 < cb && char::from(buf[idx]) == 'O' && char::from(buf[idx + 1]) == 'K')
                    || (idx + 3 < cb
                        && char::from(buf[idx]) == 'A'
                        && char::from(buf[idx + 1]) == 'C'
                        && char::from(buf[idx + 2]) == 'K')
                {
                    more = false;
                }
            }
        }

        Ok(String::from_utf8(buf)?)
    }
}

/// Utility function to parse the initial response to a connection from mpd
async fn parse_connect_rsp<T>(sock: &mut T) -> Result<String>
where
    T: AsyncRead + AsyncWrite + Send + Unpin,
{
    let mut buf = Vec::with_capacity(32);
    let _cb = sock.read_buf(&mut buf).await?;
    let text = String::from_utf8(buf)?;
    text.starts_with("OK MPD ").as_option().context(Connect {
        text: text.trim().to_string(),
    })?;
    info!("Connected {}.", text[7..].trim());
    Ok(text[7..].trim().to_string())
}

impl MpdConnection<TcpStream> {
    pub async fn new<A: ToSocketAddrs>(addr: A) -> Result<Box<dyn RequestResponse>> {
        let mut sock = TcpStream::connect(addr).await?;
        let proto_ver = parse_connect_rsp(&mut sock).await?;
        Ok(Box::new(MpdConnection::<TcpStream> {
            sock: sock,
            _proto_ver: proto_ver,
        }))
    }
}

impl MpdConnection<UnixStream> {
    pub async fn new<P: AsRef<Path>>(pth: P) -> Result<Box<dyn RequestResponse>> {
        let mut sock = UnixStream::connect(pth).await?;
        let proto_ver = parse_connect_rsp(&mut sock).await?;
        Ok(Box::new(MpdConnection::<UnixStream> {
            sock: sock,
            _proto_ver: proto_ver,
        }))
    }
}

/// Quote an argument by backslash-escaping " & \ characters
pub fn quote(text: &str) -> String {
    if text.contains(&[' ', '\t', '\'', '"'][..]) {
        let mut s = String::from("\"");
        for c in text.chars() {
            if c == '"' || c == '\\' {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    } else {
        text.to_string()
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                               Client                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// General-purpose [`mpd`] client. "General-purpose" in the sense that we send commands through
/// it; the interface is narrowly scoped to this program's needs.
///
/// Construct instances with a TCP socket, a Unix socket, or any [`RequestResponse`] implementation.
/// I hate carrying the regexp's around with each instance; ideally they'd be constructed once,
/// preferrably at compilation time, but at least a program start-up, and re-used, but I can't
/// figure out how to do that in Rust.
///
/// [`mpd`]: https://www.musicpd.org/doc/html/protocol.html
pub struct Client {
    stream: Box<dyn RequestResponse>,
    re_state: Regex,
    re_songid: Regex,
    re_elapsed: Regex,
    re_file: Regex,
    re_duration: Regex,
}

const RE_STATE: &str = r"(?m)^state: (play|pause|stop)$";
const RE_SONGID: &str = r"(?m)^songid: ([0-9]+)$";
const RE_ELAPSED: &str = r"(?m)^elapsed: ([.0-9]+)$";
const RE_FILE: &str = r"(?m)^file: (.*)$";
const RE_DURATION: &str = r"(?m)^duration: (.*)$";

impl Client {
    pub async fn connect<A: ToSocketAddrs>(addrs: A) -> Result<Client> {
        Self::new(MpdConnection::<TcpStream>::new(addrs).await?)
    }

    pub async fn open<P: AsRef<Path>>(pth: P) -> Result<Client> {
        Self::new(MpdConnection::<UnixStream>::new(pth).await?)
    }

    pub fn new(stream: Box<dyn RequestResponse>) -> Result<Client> {
        Ok(Client {
            stream: stream,
            re_state: Regex::new(&RE_STATE)?,
            re_songid: Regex::new(RE_SONGID)?,
            re_elapsed: Regex::new(RE_ELAPSED)?,
            re_file: Regex::new(RE_FILE)?,
            re_duration: Regex::new(RE_DURATION)?,
        })
    }
}

/// Utility function that applies the given regexp to `text`, and returns the first sub-group.
///
/// The two context selectors shouldn't be needed, but they don't implement Clone, and I can't
/// get snafu's `context` to borrow them.
fn cap<C1, C2>(re: &regex::Regex, text: &str, ctx1: C1, ctx2: C2) -> Result<String>
where
    C1: snafu::IntoError<Error, Source = snafu::NoneError>,
    C2: snafu::IntoError<Error, Source = snafu::NoneError>,
{
    Ok(re
        .captures(text)
        .context(ctx1)?
        .get(1)
        .context(ctx2)?
        .as_str()
        .to_string())
}

impl Client {
    /// Retrieve the current server status.
    pub async fn status(&mut self) -> Result<PlayerStatus> {
        // We begin with sending the "status" command: "Reports the current status of the player and
        // the volume level." Per the docs, "MPD may omit lines which have no (known) value", so I
        // can't really count on particular lines being there. Tho nothing is said in the docs, I
        // also don't want to depend on the order.
        let text = self.stream.req("status").await?;

        // I first thought to avoid the use (and cost) of regular expressions by just doing
        // sub-string searching on "state: ", but when I realized I needed to only match
        // at the beginning of a line bailed & just went ahead. This makes for more succinct
        // code, since I can't count on order, either.
        let state = cap(
            &self.re_state,
            &text,
            PlayState {
                text: text.to_string(),
            },
            PlayState {
                text: text.to_string(),
            },
        )?;

        match state.as_str() {
            "stop" => Ok(PlayerStatus::Stopped),
            "play" | "pause" => {
                let songid = cap(
                    &self.re_songid,
                    &text,
                    SongId {
                        text: text.to_string(),
                    },
                    SongId {
                        text: text.to_string(),
                    },
                )?
                .parse::<u64>()?;
                let elapsed = cap(
                    &self.re_elapsed,
                    &text,
                    Elapsed {
                        text: text.to_string(),
                    },
                    Elapsed {
                        text: text.to_string(),
                    },
                )?
                .parse::<f64>()?;

                // navigate from `songid'-- don't send a "currentsong" message-- the current song
                // could have changed
                let text = self.stream.req(&format!("playlistid {}", songid)).await?;

                let file = cap(
                    &self.re_file,
                    &text,
                    SongFile {
                        text: text.to_string(),
                    },
                    SongFile {
                        text: text.to_string(),
                    },
                )?;
                let duration = cap(
                    &self.re_duration,
                    &text,
                    Duration {
                        text: text.to_string(),
                    },
                    Duration {
                        text: text.to_string(),
                    },
                )?
                .parse::<f64>()?;

                let curr = CurrentSong::new(songid, PathBuf::from(file), elapsed, duration);

                if state == "play" {
                    Ok(PlayerStatus::Play(curr))
                } else {
                    Ok(PlayerStatus::Pause(curr))
                }
            }
            _ => Err(Error::UnknownState {
                state: state.to_string(),
                back: Backtrace::generate(),
            }),
        }
    }

    /// Retrieve a song sticker by name, as a string
    pub async fn get_sticker<T: FromStr>(
        &mut self,
        file: &str,
        sticker_name: &str,
    ) -> Result<Option<T>>
    where
        <T as FromStr>::Err: std::error::Error,
    {
        let msg = format!("sticker get song \"{}\" \"{}\"", file, sticker_name);
        let text = self.stream.req(&msg).await?;
        debug!("Sent message `{}'; got `{}'", &msg, &text);

        let prefix = format!("sticker: {}=", sticker_name);
        if text.starts_with(&prefix) {
            let s = text[prefix.len()..]
                .split('\n')
                .next()
                .context(StickerGet { text: text.clone() })?;
            let r = T::from_str(s);
            match r {
                Ok(t) => Ok(Some(t)),
                Err(e) => Err(Error::BadStickerConversion {
                    sticker: String::from(sticker_name),
                    error: format!("{}", e),
                    back: Backtrace::generate(),
                }),
            }
        } else if text.starts_with("ACK ") && text.ends_with("no such sticker\n") {
            Ok(None)
        } else {
            Err(Error::StickerGet {
                text: text.to_string(),
                back: Backtrace::generate(),
            })
        }
    }

    /// Set a song sticker by name, as text
    pub async fn set_sticker<T: std::fmt::Display>(
        &mut self,
        file: &str,
        sticker_name: &str,
        sticker_value: &T,
    ) -> Result<()> {
        let msg = format!(
            "sticker set song \"{}\" \"{}\" \"{}\"",
            file, sticker_name, sticker_value
        );
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'", &msg, &text);

        text.starts_with("OK").as_option().context(StickerSet {
            text: text.to_string(),
        })?;
        Ok(())
    }

    /// Send a file to a playlist
    pub async fn send_to_playlist(&mut self, file: &str, pl: &str) -> Result<()> {
        let msg = format!("playlistadd \"{}\" \"{}\"", pl, file);
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        text.starts_with("OK").as_option().context(StickerSet {
            text: text.to_string(),
        })?;
        Ok(())
    }

    /// Send an arbitrary message
    pub async fn send_message(&mut self, chan: &str, msg: &str) -> Result<()> {
        let msg = format!("sendmessage {} {}", chan, quote(msg));
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        text.starts_with("OK").as_option().context(SendMessage {
            text: text.to_string(),
        })?;
        Ok(())
    }

    /// Update a URI
    pub async fn update(&mut self, uri: &str) -> Result<u64> {
        let msg = format!("update \"{}\"", uri);
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        // We expect a response of the form:
        //   updating_db: JOBID
        //   OK
        // on success, and
        //   ACK ERR
        // on failure.

        let prefix = "updating_db: ";
        if text.starts_with(prefix) {
            // Seems prolix to me
            Ok(text[prefix.len()..].split('\n').collect::<Vec<&str>>()[0]
                .to_string()
                .parse::<u64>()?)
        } else {
            Err(Error::UpdateDB {
                text: text.to_string(),
                back: Backtrace::generate(),
            })
        }
    }

    /// Get the list of stored playlists
    pub async fn get_stored_playlists(&mut self) -> Result<std::vec::Vec<String>> {
        let text = self.stream.req("listplaylists").await?;
        debug!("Sent listplaylists; got `{}'.", &text);

        // We expect a response of the form:
        // playlist: a
        // Last-Modified: 2020-03-13T17:20:16Z
        // playlsit: b
        // Last-Modified: 2020-03-13T17:20:16Z
        // ...
        // OK
        //
        // or
        //
        // ACK...
        if text.starts_with("ACK") {
            Err(Error::GetStoredPlaylists {
                text: text.to_string(),
                back: Backtrace::generate(),
            })
        } else {
            Ok(text
                .lines()
                .filter_map(|x| {
                    if x.starts_with("playlist: ") {
                        Some(String::from(&x[10..]))
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>())
        }
    }
}

#[cfg(test)]
/// Let's test Client!
mod client_tests {

    use super::test_mock::Mock;
    use super::*;

    /// Some basic "smoke" tests
    #[tokio::test]
    async fn client_smoke_test() {
        let mock = Box::new(Mock::new(&[(
            "sticker get song \"foo.mp3\" \"stick\"",
            "sticker: stick=splat\nOK\n",
        )]));
        let mut cli = Client::new(mock).unwrap();
        let val = cli
            .get_sticker::<String>("foo.mp3", "stick")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(val, "splat");
    }

    /// Test the `status' method
    #[tokio::test]
    async fn test_status() {
        let mock = Box::new(Mock::new(&[
            (
                "status",
                // When the server is playing or paused, the response will look something like this:
                "volume: -1
repeat: 0
random: 0
single: 0
consume: 0
playlist: 3
playlistlength: 87
mixrampdb: 0.000000
state: play
song: 14
songid: 15
time: 141:250
bitrate: 128
audio: 44100:24:2
nextsong: 15
nextsongid: 16
elapsed: 140.585
OK",
            ),
            // Should respond with a playlist id request
            (
                "playlistid 15",
                // Should look something like this:
                "file: U-Z/U2 - Who's Gonna RIDE Your WILD HORSES.mp3
Last-Modified: 2004-12-24T19:26:13Z
Artist: U2
Title: Who's Gonna RIDE Your WILD HOR
Genre: Pop
Time: 316
Pos: 41
Id: 42
duration: 249.994
OK",
            ),
            (
                "status",
                // But if the state is "stop", much of that will be missing; it will look more like:
                "volume: -1
repeat: 0
random: 0
single: 0
consume: 0
playlist: 84
playlistlength: 27
mixrampdb: 0.000000
state: stop
OK",
            ),
            // Finally, let's simulate something being really wrong
            (
                "status",
                "volume: -1
repeat: 0
state: no-idea!?",
            ),
        ]));
        let mut cli = Client::new(mock).unwrap();
        let stat = cli.status().await.unwrap();
        match stat {
            PlayerStatus::Play(curr) => {
                assert_eq!(curr.songid, 15);
                assert_eq!(
                    curr.file.to_str().unwrap(),
                    "U-Z/U2 - Who's Gonna RIDE Your WILD HORSES.mp3"
                );
                assert_eq!(curr.elapsed, 140.585);
                assert_eq!(curr.duration, 249.994);
            }
            _ => panic!(),
        }

        let stat = cli.status().await.unwrap();
        match stat {
            PlayerStatus::Stopped => (),
            _ => panic!(),
        }

        let stat = cli.status().await;
        match stat {
            Err(_) => (),
            Ok(_) => panic!(),
        }
    }

    /// Test the `get_sticker' method
    #[tokio::test]
    async fn test_get_sticker() {
        let mock = Box::new(Mock::new(&[
            (
                "sticker get song \"foo.mp3\" \"stick\"",
                // On success, should get something like this...
                "sticker: stick=2\nOK\n",
            ),
            (
                "sticker get song \"foo.mp3\" \"stick\"",
                // and on failure, something like this:
                "ACK [50@0] {sticker} no such sticker\n",
            ),
            (
                "sticker get song \"foo.mp3\" \"stick\"",
                // Finally, let's try something nuts
                "",
            ),
        ]));
        let mut cli = Client::new(mock).unwrap();
        let val = cli
            .get_sticker::<String>("foo.mp3", "stick")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(val, "2");
        let _val = cli
            .get_sticker::<String>("foo.mp3", "stick")
            .await
            .unwrap()
            .is_none();
        let _val = cli
            .get_sticker::<String>("foo.mp3", "stick")
            .await
            .unwrap_err();
    }

    /// Test the `set_sticker' method
    #[tokio::test]
    async fn test_set_sticker() {
        let mock = Box::new(Mock::new(&[
            ("sticker set song \"foo.mp3\" \"stick\" \"2\"", "OK\n"),
            (
                "sticker set song \"foo.mp3\" \"stick\" \"2\"",
                "ACK [50@0] {sticker} some error",
            ),
            (
                "sticker set song \"foo.mp3\" \"stick\" \"2\"",
                "this makes no sense as a response",
            ),
        ]));
        let mut cli = Client::new(mock).unwrap();
        let _val = cli.set_sticker("foo.mp3", "stick", &"2").await.unwrap();
        let _val = cli.set_sticker("foo.mp3", "stick", &"2").await.unwrap_err();
        let _val = cli.set_sticker("foo.mp3", "stick", &"2").await.unwrap_err();
    }

    /// Test the `send_to_playlist' method
    #[tokio::test]
    async fn test_send_to_playlist() {
        let mock = Box::new(Mock::new(&[
            ("playlistadd \"foo.m3u\" \"foo.mp3\"", "OK\n"),
            (
                "playlistadd \"foo.m3u\" \"foo.mp3\"",
                "ACK [101@0] {playlist} some error\n",
            ),
        ]));
        let mut cli = Client::new(mock).unwrap();
        let _val = cli.send_to_playlist("foo.mp3", "foo.m3u").await.unwrap();
        let _val = cli
            .send_to_playlist("foo.mp3", "foo.m3u")
            .await
            .unwrap_err();
    }

    /// Test the `update' method
    #[tokio::test]
    async fn test_update() {
        let mock = Box::new(Mock::new(&[
            ("update \"foo.mp3\"", "updating_db: 2\nOK\n"),
            ("update \"foo.mp3\"", "ACK [50@0] {update} blahblahblah"),
            ("update \"foo.mp3\"", "this makes no sense as a response"),
        ]));
        let mut cli = Client::new(mock).unwrap();
        let _val = cli.update("foo.mp3").await.unwrap();
        let _val = cli.update("foo.mp3").await.unwrap_err();
        let _val = cli.update("foo.mp3").await.unwrap_err();
    }

    /// Test retrieving stored playlists
    #[tokio::test]
    async fn test_get_stored_playlists() {
        let mock = Box::new(Mock::new(&[
            (
                "listplaylists",
                "playlist: saturday-afternoons-in-santa-cruz
Last-Modified: 2020-03-13T17:20:16Z
playlist: gaelic-punk
Last-Modified: 2020-05-24T00:36:02Z
playlist: morning-coffee
Last-Modified: 2020-03-13T17:20:16Z
OK
",
            ),
            ("listplaylists", "ACK [1@0] {listplaylists} blahblahblah"),
        ]));

        let mut cli = Client::new(mock).unwrap();
        let val = cli.get_stored_playlists().await.unwrap();
        assert_eq!(
            val,
            vec![
                String::from("saturday-afternoons-in-santa-cruz"),
                String::from("gaelic-punk"),
                String::from("morning-coffee")
            ]
        );
        let _val = cli.get_stored_playlists().await.unwrap_err();
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           IdleClient                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, PartialEq, Eq)]
pub enum IdleSubSystem {
    Player,
    Message,
}

impl TryFrom<&str> for IdleSubSystem {
    type Error = Error;
    fn try_from(text: &str) -> std::result::Result<Self, Self::Error> {
        let x = text.to_lowercase();
        if x == "player" {
            Ok(IdleSubSystem::Player)
        } else if x == "message" {
            Ok(IdleSubSystem::Message)
        } else {
            Err(Error::IdleSubsystem {
                text: String::from(text),
                back: Backtrace::generate(),
            })
        }
    }
}

impl fmt::Display for IdleSubSystem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            IdleSubSystem::Player => write!(f, "Player"),
            IdleSubSystem::Message => write!(f, "Message"),
        }
    }
}

/// mpdpopm client for "idle" connections.
///
/// I should probably make this generic over all types implementing AsyncRead & AsyncWrite.
pub struct IdleClient {
    conn: Box<dyn RequestResponse>,
}

impl IdleClient {
    /// Create a new [`mpdpopm::client::IdleClient`][`IdleClient`] instance from something that
    /// implements [`ToSocketAddrs`][`tokio::net::ToSocketAddrs`]
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<IdleClient> {
        Self::new(MpdConnection::<TcpStream>::new(addr).await?)
    }

    pub async fn open<P: AsRef<Path>>(pth: P) -> Result<IdleClient> {
        Self::new(MpdConnection::<UnixStream>::new(pth).await?)
    }

    pub fn new(stream: Box<dyn RequestResponse>) -> Result<IdleClient> {
        Ok(IdleClient { conn: stream })
    }

    /// Subscribe to an mpd channel
    pub async fn subscribe(&mut self, chan: &str) -> Result<()> {
        let text = self.conn.req(&format!("subscribe {}", chan)).await?;
        debug!("Sent subscribe message for {}; got `{}'.", chan, text);
        text.starts_with("OK").as_option().context(Subscribe {
            text: String::from(text),
        })?;
        debug!("Subscribed to {}.", chan);
        Ok(())
    }

    /// Enter idle state-- return the subsystem that changed, causing the connection to return. NB
    /// this may block for some time.
    pub async fn idle(&mut self) -> Result<IdleSubSystem> {
        let text = self.conn.req("idle player message").await?;
        debug!("Sent idle message; got `{}'.", text);

        // If the player state changes, we'll get: "changed: player\nOK\n"
        //
        // If a ratings message is sent, we'll get: "changed: message\nOK\n", to which we respond
        // "readmessages", which should give us something like:
        //
        //     channel: ratings
        //     message: 255
        //     OK
        //
        // We remain subscribed, but we need to send a new idle message.

        text.starts_with("changed: ").as_option().context(Idle {
            text: String::from(&text),
        })?;
        let idx = text.find('\n').context(Idle {
            text: String::from(&text),
        })?;
        let result = IdleSubSystem::try_from(&text[9..idx])?;
        let text = text[idx + 1..].to_string();
        text.starts_with("OK").as_option().context(Idle {
            text: String::from(text),
        })?;

        Ok(result)
    }

    /// This method simply returns the results of a "readmessages" as a HashMap of channel name to
    /// Vec of (String) messages for that channel
    pub async fn get_messages(&mut self) -> Result<HashMap<String, Vec<String>>> {
        let text = self.conn.req("readmessages").await?;
        debug!("Sent readmessages; got `{}'.", text);

        // We expect something like:
        //
        //     channel: ratings
        //     message: 255
        //     OK
        //
        // We remain subscribed, but we need to send a new idle message.

        let mut m: HashMap<String, Vec<String>> = HashMap::new();

        // Populate `m' with a little state machine:
        enum State {
            Init,
            Running,
            Finished,
        };
        let mut state = State::Init;
        let mut chan = String::new();
        let mut msgs: Vec<String> = Vec::new();
        for line in text.lines() {
            match state {
                State::Init => {
                    line.starts_with("channel: ")
                        .as_option()
                        .context(GetMessages {
                            text: String::from(line),
                        })?;
                    chan = String::from(&line[9..]);
                    state = State::Running;
                }
                State::Running => {
                    if line.starts_with("message: ") {
                        msgs.push(String::from(&line[9..]));
                    } else if line.starts_with("channel: ") {
                        match m.get_mut(&chan) {
                            Some(v) => v.append(&mut msgs),
                            None => {
                                m.insert(chan.clone(), msgs.clone());
                            }
                        }
                        chan = String::from(&line[9..]);
                        msgs = Vec::new();
                    } else if line == "OK" {
                        match m.get_mut(&chan) {
                            Some(v) => v.append(&mut msgs),
                            None => {
                                m.insert(chan.clone(), msgs.clone());
                            }
                        }
                        state = State::Finished;
                    } else {
                        return Err(Error::GetMessages {
                            text: String::from(line),
                            back: Backtrace::generate(),
                        });
                    }
                }
                State::Finished => {
                    // Should never be here!
                    return Err(Error::GetMessages {
                        text: String::from(line),
                        back: Backtrace::generate(),
                    });
                }
            }
        }

        Ok(m)
    }
}

#[cfg(test)]
/// Let's test IdleClient!
mod idle_client_tests {

    use super::test_mock::Mock;
    use super::*;

    /// Some basic "smoke" tests
    #[tokio::test]
    async fn test_get_messages() {
        let mock = Box::new(Mock::new(&[(
            "readmessages",
            // If a ratings message is sent, we'll get: "changed: message\nOK\n", to which we
            // respond "readmessages", which should give us something like:
            //
            //     channel: ratings
            //     message: 255
            //     OK
            //
            // We remain subscribed, but we need to send a new idle message.
            "channel: ratings
message: 255
message: 128
channel: send-to-playlist
message: foo.m3u
OK
",
        )]));
        let mut cli = IdleClient::new(mock).unwrap();
        let hm = cli.get_messages().await.unwrap();
        let val = hm.get("ratings").unwrap();
        assert_eq!(val.len(), 2);
        let val = hm.get("send-to-playlist").unwrap();
        assert!(val.len() == 1);
    }

    /// Test issue #1
    #[tokio::test]
    async fn test_issue_1() {
        let mock = Box::new(Mock::new(&[(
            "readmessages",
            "channel: playcounts
message: a
channel: playcounts
message: b
OK
",
        )]));
        let mut cli = IdleClient::new(mock).unwrap();
        let hm = cli.get_messages().await.unwrap();
        let val = hm.get("playcounts").unwrap();
        assert_eq!(val.len(), 2);
    }
}
