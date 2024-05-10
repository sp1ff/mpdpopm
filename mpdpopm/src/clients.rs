// Copyright (C) 2020-2024 Michael Herstine <sp1ff@pobox.com>
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
//! This module contains basic types implementing various MPD client operations (cf. the [mpd
//! protocol](http://www.musicpd.org/doc/protocol/)). Since issuing the "idle" command will tie up
//! the connection, MPD clients often use multiple connections to the server (one to listen for
//! updates, one or more on which to issue commands). This modules provides two different client
//! types: [Client] for general-purpose use and [IdleClient] for long-lived connections listening
//! for server notifiations.
//!
//! Note that there *is* another idiom (used in [libmpdel](https://github.com/mpdel/libmpdel),
//! e.g.): open a single connection & issue an "idle" command. When you want to issue a command,
//! send a "noidle", then the command, then "idle" again.  This isn't a race condition, as the
//! server will buffer any changes that took place when you were not idle & send them when you
//! re-issue the "idle" command. This crate however takes the approach of two channels (like
//! [mpdfav](https://github.com/vincent-petithory/mpdfav)).

use async_trait::async_trait;
use backtrace::Backtrace;
use log::{debug, info};
use regex::Regex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs, UnixStream};

use lazy_static::lazy_static;

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

// The Protocol error, below, gets used a *lot*; anywhere we receive a message from the MPD server
// that "should" never happen. To help give a bit of context beyond a stack trace, I use this
// enumeration of "operations"
/// Enumerated list of MPD operations; used in Error::Protocol to distinguish which operation it was
/// that elicited the protocol error.
#[derive(Debug)]
#[non_exhaustive]
pub enum Operation {
    Connect,
    Status,
    GetSticker,
    SetSticker,
    SendToPlaylist,
    SendMessage,
    Update,
    GetStoredPlaylists,
    RspToUris,
    GetStickers,
    GetAllSongs,
    Add,
    Idle,
    GetMessages,
}

impl std::fmt::Display for Operation {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Operation::Connect => write!(f, "Connect"),
            Operation::Status => write!(f, "Status"),
            Operation::GetSticker => write!(f, "GetSticker"),
            Operation::SetSticker => write!(f, "SetSticker"),
            Operation::SendToPlaylist => write!(f, "SendToPlaylist"),
            Operation::SendMessage => write!(f, "SendMessage"),
            Operation::Update => write!(f, "Update"),
            Operation::GetStoredPlaylists => write!(f, "GetStoredPlaylists"),
            Operation::RspToUris => write!(f, "RspToUris"),
            Operation::GetStickers => write!(f, "GetStickers"),
            Operation::GetAllSongs => write!(f, "GetAllSongs"),
            Operation::Add => write!(f, "Add"),
            Operation::Idle => write!(f, "Idle"),
            Operation::GetMessages => write!(f, "GetMessages"),
            _ => write!(f, "Unknown client operation"),
        }
    }
}

/// An MPD client error
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Protocol {
        op: Operation,
        msg: String,
        back: backtrace::Backtrace,
    },
    Io {
        source: std::io::Error,
        back: backtrace::Backtrace,
    },
    Encoding {
        source: std::string::FromUtf8Error,
        back: backtrace::Backtrace,
    },
    StickerConversion {
        sticker: String,
        source: Box<dyn std::error::Error + Send + Sync>,
        back: backtrace::Backtrace,
    },
    IdleSubSystem {
        text: String,
        back: backtrace::Backtrace,
    },
}

impl std::convert::From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io {
            source: err,
            back: Backtrace::new(),
        }
    }
}

// Implementation helper: there is a lot of code in this module that reads: "(do thing that returns
// Option).ok_or_else(|| create an Error::Protocol instance).and then..." This private trait helps
// make that a bit more succinct. I largely lifted it from Snafu.
trait OptionExt<T>: Sized {
    fn context(self, op: Operation, msg: &str) -> std::result::Result<T, Error>;
}

impl<T> OptionExt<T> for Option<T> {
    fn context(self, op: Operation, msg: &str) -> std::result::Result<T, Error> {
        self.ok_or_else(|| Error::Protocol {
            op: op,
            msg: String::from(msg), // msg is likely borrowed, sadly
            back: Backtrace::new(),
        })
    }
}

// This impl lets you change a bool into an Error::Protocol, as well: "(boolean
// assertion).context(...)"
impl OptionExt<()> for bool {
    fn context(self, op: Operation, msg: &str) -> std::result::Result<(), Error> {
        if self {
            Ok(())
        } else {
            Err(Error::Protocol {
                op: op,
                msg: String::from(msg),
                back: Backtrace::new(),
            })
        }
    }
}

// Same for Result-s!
trait ResultExt<T>: Sized {
    fn context(self, op: Operation, msg: &str) -> std::result::Result<T, Error>;
}

impl<T, E> ResultExt<T> for std::result::Result<T, E> {
    fn context(self, op: Operation, msg: &str) -> std::result::Result<T, Error> {
        self.map_err(|_| Error::Protocol {
            op: op,
            msg: String::from(msg),
            back: Backtrace::new(),
        })
    }
}

impl std::fmt::Display for Error {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Protocol { op, msg, back: _ } => write!(f, "Protocol error ({}): {}", op, msg),
            Error::Io { source, back: _ } => write!(f, "I/O error: {}", source),
            Error::Encoding { source, back: _ } => write!(f, "Encoding error: {}", source),
            Error::StickerConversion {
                sticker,
                source,
                back: _,
            } => write!(f, "While converting sticker ``{}'': {}", sticker, source),
            Error::IdleSubSystem { text, back: _ } => {
                write!(f, "``{}'' is not a recognized Idle subsystem", text)
            }
            _ => write!(f, "Unknown client error"),
        }
    }
}

// Stolen shamelessly from Snafu
// <https://github.com/shepmaster/snafu/blob/c5255d3ae1e4af1e8b7fd34298f80f9f2399b9fd/src/lib.rs#L991>
// this trait turns the receiver into an Error trait object.
pub trait AsErrorSource {
    /// For maximum effectiveness, this needs to be called as a method
    /// to benefit from Rust's automatic dereferencing of method
    /// receivers.
    fn as_error_source(&self) -> &(dyn std::error::Error + 'static);
}

impl AsErrorSource for dyn std::error::Error + Send + Sync + 'static {
    fn as_error_source(&self) -> &(dyn std::error::Error + 'static) {
        self
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self {
            Error::Io {
                ref source,
                back: _,
            } => Some(source),
            Error::Encoding {
                ref source,
                back: _,
            } => Some(source),
            Error::StickerConversion {
                sticker: _,
                ref source,
                back: _,
            } => Some(source.as_error_source()),
            _ => None,
        }
    }
}

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

/// The MPD player itself can be in one of three states: playing, paused or stopped. In the first
/// two there is a "current" song.
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

/// A trait representing a simple, textual request/response protocol like that
/// [employed](https://www.musicpd.org/doc/html/protocol.html) by [MPD](https://www.musicpd.org/):
/// the caller sends a textual command & the server responds with a (perhaps multi-line) textual
/// response.
///
/// This trait also enables unit testing client implementations. Note that it is async-- cf.
/// [async_trait](https://docs.rs/async-trait/latest/async_trait/).
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

/// [MPD](https://www.musicpd.org/) connections talk the same
/// [protocol](https://www.musicpd.org/doc/html/protocol.html) over either a TCP or a Unix socket.
///
/// # Examples
///
/// Implementations are provided for tokio [UnixStream] and [TcpStream], but [MpdConnection] is a
/// trait that can work in terms of any asynchronous communications channel (so long as it is also
/// [Send] and [Unpin] so async executors can pass them between threads.
///
/// To create a connection to an `MPD` server over a Unix domain socket:
///
/// ```no_run
/// use std::path::Path;
/// use tokio::net::UnixStream;
/// use mpdpopm::clients::MpdConnection;
/// let local_conn = MpdConnection::<UnixStream>::new(Path::new("/var/run/mpd/mpd.sock"));
/// ```
///
/// In this example, `local_conn` is a Future that will resolve to a Result containing the
/// [MpdConnection] Unix domain socket implementation once the socket has been established, the MPD
/// server greets us & the protocol version has been parsed.
///
/// or over a TCP socket:
///
/// ```no_run
/// use std::net::SocketAddrV4;
/// use tokio::net::{TcpStream, ToSocketAddrs};
/// use mpdpopm::clients::MpdConnection;
/// let tcp_conn = MpdConnection::<TcpStream>::new("localhost:6600".parse::<SocketAddrV4>().unwrap());
/// ```
///
/// Here, `tcp_conn` is a Future that will resolve to a Result containing the [MpdConnection] TCP
/// implementation on successful connection to the MPD server (i.e. the connection is established,
/// the server greets us & we parse the protocol version).
///
///
pub struct MpdConnection<T: AsyncRead + AsyncWrite + Send + Unpin> {
    sock: T,
    _protocol_ver: String,
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

        Ok(String::from_utf8(buf).map_err(|err| Error::Encoding {
            source: err,
            back: Backtrace::new(),
        })?)
    }
}

/// Utility function to parse the initial response to a connection from mpd
async fn parse_connect_rsp<T>(sock: &mut T) -> Result<String>
where
    T: AsyncReadExt + AsyncWriteExt + Send + Unpin,
{
    let mut buf = Vec::with_capacity(32);
    let _cb = sock.read_buf(&mut buf).await?;
    let text = String::from_utf8(buf).map_err(|err| Error::Encoding {
        source: err,
        back: Backtrace::new(),
    })?;
    text.starts_with("OK MPD ")
        .context(Operation::Connect, text.trim())?;
    info!("Connected {}.", text[7..].trim());
    Ok(text[7..].trim().to_string())
}

impl MpdConnection<TcpStream> {
    pub async fn new<A: ToSocketAddrs>(addr: A) -> Result<Box<dyn RequestResponse>> {
        let mut sock = TcpStream::connect(addr).await?;
        let proto_ver = parse_connect_rsp(&mut sock).await?;
        Ok(Box::new(MpdConnection::<TcpStream> {
            sock: sock,
            _protocol_ver: proto_ver,
        }))
    }
}

impl MpdConnection<UnixStream> {
    // NTS: we have to box the return value because a `dyn RequestResponse` isn't Sized.
    pub async fn new<P: AsRef<Path>>(pth: P) -> Result<Box<dyn RequestResponse>> {
        let mut sock = UnixStream::connect(pth).await?;
        let proto_ver = parse_connect_rsp(&mut sock).await?;
        Ok(Box::new(MpdConnection::<UnixStream> {
            sock: sock,
            _protocol_ver: proto_ver,
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

/// General-purpose [mpd](https://www.musicpd.org)
/// [client](https://www.musicpd.org/doc/html/protocol.html): "general-purpose" in the sense that we
/// send commands through it; the interface is narrowly scoped to this program's needs.
///
/// # Introduction
///
/// This is the primary abstraction of the MPD client protocol, written for the convenience of
/// [mpdpopm](crate). Construct instances with a TCP socket, a Unix socket, or any [RequestResponse]
/// implementation. You can then carry out assorted operations in the MPD client protocol by
/// invoking its methods.
///
/// ```no_run
/// use std::path::Path;
/// use mpdpopm::clients::Client;
/// let client = Client::open(Path::new("/var/run/mpd.sock"));
/// ```
///
/// `client` is now a [Future](https://doc.rust-lang.org/stable/std/future/trait.Future.html) that
/// resolves to a [Client] instance talking to `/var/run/mpd.sock`.
///
/// ```no_run
/// use mpdpopm::clients::Client;
/// let client = Client::connect("localhost:6600");
/// ```
///
/// `client` is now a [Future](https://doc.rust-lang.org/stable/std/future/trait.Future.html) that
/// resolves to a [Client] instance talking TCP to the MPD server on localhost at port 6600.
pub struct Client {
    stream: Box<dyn RequestResponse>,
}

// Thanks to <https://stackoverflow.com/questions/35169259/how-to-make-a-compiled-regexp-a-global-variable>
lazy_static! {
    static ref RE_STATE: regex::Regex = Regex::new(r"(?m)^state: (play|pause|stop)$").unwrap();
    static ref RE_SONGID: regex::Regex = Regex::new(r"(?m)^songid: ([0-9]+)$").unwrap();
    static ref RE_ELAPSED: regex::Regex = Regex::new(r"(?m)^elapsed: ([.0-9]+)$").unwrap();
    static ref RE_FILE: regex::Regex = Regex::new(r"(?m)^file: (.*)$").unwrap();
    static ref RE_DURATION: regex::Regex = Regex::new(r"(?m)^duration: (.*)$").unwrap();
}

impl Client {
    pub async fn connect<A: ToSocketAddrs>(addrs: A) -> Result<Client> {
        Self::new(MpdConnection::<TcpStream>::new(addrs).await?)
    }

    pub async fn open<P: AsRef<Path>>(pth: P) -> Result<Client> {
        Self::new(MpdConnection::<UnixStream>::new(pth).await?)
    }

    pub fn new(stream: Box<dyn RequestResponse>) -> Result<Client> {
        Ok(Client { stream: stream })
    }
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
        // sub-string searching on "state: ", but when I realized I needed to only match at the
        // beginning of a line I bailed & just went ahead. This makes for more succinct code, since
        // I can't count on order, either.
        let state = RE_STATE
            .captures(&text)
            .context(Operation::Status, &text)?
            .get(1)
            .context(Operation::Status, &text)?
            .as_str();

        match state {
            "stop" => Ok(PlayerStatus::Stopped),
            "play" | "pause" => {
                let songid = RE_SONGID
                    .captures(&text)
                    .context(Operation::Status, &text)?
                    .get(1)
                    .context(Operation::Status, &text)?
                    .as_str()
                    .parse::<u64>()
                    .context(Operation::Status, &text)?;

                let elapsed = RE_ELAPSED
                    .captures(&text)
                    .context(Operation::Status, &text)?
                    .get(1)
                    .context(Operation::Status, &text)?
                    .as_str()
                    .parse::<f64>()
                    .context(Operation::Status, &text)?;

                // navigate from `songid'-- don't send a "currentsong" message-- the current song
                // could have changed
                let text = self.stream.req(&format!("playlistid {}", songid)).await?;

                let file = RE_FILE
                    .captures(&text)
                    .context(Operation::Status, &text)?
                    .get(1)
                    .context(Operation::Status, &text)?
                    .as_str();
                let duration = RE_DURATION
                    .captures(&text)
                    .context(Operation::Status, &text)?
                    .get(1)
                    .context(Operation::Status, &text)?
                    .as_str()
                    .parse::<f64>()
                    .context(Operation::Status, &text)?;

                let curr = CurrentSong::new(songid, PathBuf::from(file), elapsed, duration);

                if state == "play" {
                    Ok(PlayerStatus::Play(curr))
                } else {
                    Ok(PlayerStatus::Pause(curr))
                }
            }
            _ => Err(Error::Protocol {
                op: Operation::Status,
                msg: state.to_string(),
                back: Backtrace::new(),
            }),
        }
    }

    /// Retrieve a song sticker by name
    pub async fn get_sticker<T: FromStr>(
        &mut self,
        file: &str,
        sticker_name: &str,
    ) -> Result<Option<T>>
    where
        <T as FromStr>::Err: std::error::Error + Sync + Send + 'static,
    {
        let msg = format!("sticker get song {} {}", quote(file), quote(sticker_name));
        let text = self.stream.req(&msg).await?;
        debug!("Sent message `{}'; got `{}'", &msg, &text);

        let prefix = format!("sticker: {}=", sticker_name);
        if text.starts_with(&prefix) {
            let s = text[prefix.len()..]
                .split('\n')
                .next()
                .context(Operation::GetSticker, &msg)?;
            Ok(Some(T::from_str(s).map_err(|e| {
                Error::StickerConversion {
                    sticker: String::from(sticker_name),
                    source: Box::<dyn std::error::Error + Sync + Send>::from(e),
                    back: Backtrace::new(),
                }
            })?))
        } else {
            // ACK_ERROR_NO_EXIST = 50 (Ack.hxx:17)
            (text.starts_with("ACK [50@0]")).context(Operation::GetSticker, &msg)?;
            Ok(None)
        }
    }

    /// Set a song sticker by name
    pub async fn set_sticker<T: std::fmt::Display>(
        &mut self,
        file: &str,
        sticker_name: &str,
        sticker_value: &T,
    ) -> Result<()> {
        let value_as_str = format!("{}", sticker_value);
        let msg = format!(
            "sticker set song {} {} {}",
            quote(file),
            quote(sticker_name),
            quote(&value_as_str)
        );
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'", &msg, &text);

        text.starts_with("OK").context(Operation::SetSticker, &msg)
    }

    /// Send a file to a playlist
    pub async fn send_to_playlist(&mut self, file: &str, pl: &str) -> Result<()> {
        let msg = format!("playlistadd {} {}", quote(pl), quote(file));
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        text.starts_with("OK")
            .context(Operation::SendToPlaylist, &msg)
    }

    /// Send an arbitrary message
    pub async fn send_message(&mut self, chan: &str, msg: &str) -> Result<()> {
        let msg = format!("sendmessage {} {}", chan, quote(msg));
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        text.starts_with("OK")
            .context(Operation::SendMessage, &text)
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
        text.starts_with(prefix).context(Operation::Update, &text)?;
        text[prefix.len()..].split('\n').collect::<Vec<&str>>()[0]
            .to_string()
            .parse::<u64>()
            .context(Operation::Update, &text)
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
            Err(Error::Protocol {
                op: Operation::GetStoredPlaylists,
                msg: text,
                back: Backtrace::new(),
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

    /// Process a search (either find or search) response
    fn search_rsp_to_uris(&self, text: &str) -> Result<std::vec::Vec<String>> {
        // We expect a response of the form:
        // file: P/Pogues, The - A Pistol For Paddy Garcia.mp3
        // Last-Modified: 2007-12-26T19:18:00Z
        // Format: 44100:24:2
        // ...
        // file: P/Pogues, The - Billy's Bones.mp3
        // ...
        // OK
        //
        // or
        //
        // ACK...
        if text.starts_with("ACK") {
            Err(Error::Protocol {
                op: Operation::RspToUris,
                msg: String::from(text),
                back: Backtrace::new(),
            })
        } else {
            Ok(text
                .lines()
                .filter_map(|x| {
                    if x.starts_with("file: ") {
                        Some(String::from(&x[6..]))
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>())
        }
    }

    /// Search the database for songs matching filter (unary operator)
    ///
    /// Set `case` to true to request a case-sensitive search (false yields case-insensitive)
    pub async fn find1(
        &mut self,
        cond: &str,
        val: &str,
        case: bool,
    ) -> Result<std::vec::Vec<String>> {
        let cmd = format!(
            "{} {}",
            if case { "find" } else { "search" },
            quote(&format!("({} {})", cond, val))
        );
        let text = self.stream.req(&cmd).await?;
        self.search_rsp_to_uris(&text)
    }

    /// Search the database for songs matching filter (case-sensitive, binary operator)
    ///
    /// Set `case` to true to request a case-sensitive search (false yields case-insensitive)
    pub async fn find2(
        &mut self,
        attr: &str,
        op: &str,
        val: &str,
        case: bool,
    ) -> Result<std::vec::Vec<String>> {
        let cmd = format!(
            "{} {}",
            if case { "find" } else { "search" },
            quote(&format!("({} {} {})", attr, op, val))
        );
        debug!("find2 sending ``{}''", cmd);
        let text = self.stream.req(&cmd).await?;
        self.search_rsp_to_uris(&text)
    }

    /// Retrieve all instances of a given sticker under the music directory
    ///
    /// Return a mapping from song URI to textual sticker value
    pub async fn get_stickers(&mut self, sticker: &str) -> Result<HashMap<String, String>> {
        let text = self
            .stream
            .req(&format!("sticker find song \"\" {}", sticker))
            .await?;

        // We expect a response of the form:
        //
        // file: U-Z/Zafari - Addis Adaba.mp3
        // sticker: unwoundstack.com:rating=64
        // ...
        // file: U-Z/Zero 7 - In Time (Album Version).mp3
        // sticker: unwoundstack.com:rating=255
        // OK
        //
        // or
        //
        // ACK ...

        if text.starts_with("ACK") {
            Err(Error::Protocol {
                op: Operation::GetStickers,
                msg: text,
                back: Backtrace::new(),
            })
        } else {
            let mut m = HashMap::new();
            let mut lines = text.lines();
            loop {
                let file = lines.next().context(Operation::GetStickers, &text)?;
                if "OK" == file {
                    break;
                }
                let val = lines.next().context(Operation::GetStickers, &text)?;

                m.insert(
                    String::from(&file[6..]),
                    String::from(&val[10 + sticker.len()..]),
                );
            }
            Ok(m)
        }
    }

    /// Retrieve the song URIs of all songs in the database
    ///
    /// Returns a vector of String
    pub async fn get_all_songs(&mut self) -> Result<std::vec::Vec<String>> {
        let text = self.stream.req("find \"(base '')\"").await?;
        // We expect a response of the form:
        // file: 0-A/A Positive Life - Lighten Up!.mp3
        // Last-Modified: 2020-11-18T22:47:07Z
        // Format: 44100:24:2
        // Time: 399
        // duration: 398.550
        // Artist: A Positive Life
        // Title: Lighten Up!
        // Genre: Electronic
        // file: 0-A/A Positive Life - Pleidean Communication.mp3
        // ...
        // OK
        //
        // or "ACK..."
        if text.starts_with("ACK") {
            Err(Error::Protocol {
                op: Operation::GetAllSongs,
                msg: text,
                back: Backtrace::new(),
            })
        } else {
            Ok(text
                .lines()
                .filter_map(|x| {
                    if x.starts_with("file: ") {
                        Some(String::from(&x[6..]))
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>())
        }
    }

    pub async fn add(&mut self, uri: &str) -> Result<()> {
        let msg = format!("add {}", quote(uri));
        let text = self.stream.req(&msg).await?;
        debug!("Sent `{}'; got `{}'.", &msg, &text);

        text.starts_with("OK").context(Operation::Add, &text)?;
        Ok(())
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
            "sticker get song foo.mp3 stick",
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
                "sticker get song foo.mp3 stick",
                // On success, should get something like this...
                "sticker: stick=2\nOK\n",
            ),
            (
                "sticker get song foo.mp3 stick",
                // and on failure, something like this:
                "ACK [50@0] {sticker} no such sticker\n",
            ),
            (
                "sticker get song foo.mp3 stick",
                // Finally, let's try something nuts
                "",
            ),
            (
                "sticker get song \"filename_with\\\"doublequotes\\\".flac\" unwoundstack.com:playcount",
                "sticker: unwoundstack.com:playcount=11\nOK\n",
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
        let val = cli
            .get_sticker::<String>(
                "filename_with\"doublequotes\".flac",
                "unwoundstack.com:playcount",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(val, "11");
    }

    /// Test the `set_sticker' method
    #[tokio::test]
    async fn test_set_sticker() {
        let mock = Box::new(Mock::new(&[
            ("sticker set song foo.mp3 stick 2", "OK\n"),
            (
                "sticker set song foo.mp3 stick 2",
                "ACK [50@0] {sticker} some error",
            ),
            (
                "sticker set song foo.mp3 stick 2",
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
            ("playlistadd foo.m3u foo.mp3", "OK\n"),
            (
                "playlistadd foo.m3u foo.mp3",
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

#[non_exhaustive]
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
            Err(Error::IdleSubSystem {
                text: String::from(text),
                back: Backtrace::new(),
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

/// [MPD](https://www.musicpd.org) client for "idle" connections.
///
/// # Introduction
///
/// This is an MPD client designed to "idle": it opens a long-lived connection to the MPD server and
/// waits for MPD to respond with a message indicating that there's been a change to a subsystem of
/// interest. At present, there are only two subsystems in which [mpdpopm](crate) is interested: the player
/// & messages (cf. [IdleSubSystem]).
///
/// ```no_run
/// use std::path::Path;
/// use tokio::runtime::Runtime;
/// use mpdpopm::clients::IdleClient;
///
/// let mut rt = Runtime::new().unwrap();
/// rt.block_on( async {
///     let mut client = IdleClient::open(Path::new("/var/run/mpd.sock")).await.unwrap();
///     client.subscribe("player").await.unwrap();
///     client.idle().await.unwrap();
///     // Arrives here when the player's state changes
/// })
/// ```
///
/// `client` is now a [Future](https://doc.rust-lang.org/stable/std/future/trait.Future.html) that
/// resolves to an [IdleClient] instance talking to `/var/run/mpd.sock`.
///
pub struct IdleClient {
    conn: Box<dyn RequestResponse>,
}

impl IdleClient {
    /// Create a new [mpdpopm::client::IdleClient][IdleClient] instance from something that
    /// implements [ToSocketAddrs][tokio::net::ToSocketAddrs]
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
        text.starts_with("OK").context(Operation::Connect, &text)?;
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

        text.starts_with("changed: ")
            .context(Operation::Idle, &text)?;
        let idx = text.find('\n').context(Operation::Idle, &text)?;

        let result = IdleSubSystem::try_from(&text[9..idx])?;
        let text = text[idx + 1..].to_string();
        text.starts_with("OK").context(Operation::Idle, &text)?;

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
        }
        let mut state = State::Init;
        let mut chan = String::new();
        let mut msgs: Vec<String> = Vec::new();
        for line in text.lines() {
            match state {
                State::Init => {
                    line.starts_with("channel: ")
                        .context(Operation::GetMessages, &line)?;
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
                        return Err(Error::Protocol {
                            op: Operation::GetMessages,
                            msg: text,
                            back: Backtrace::new(),
                        });
                    }
                }
                State::Finished => {
                    // Should never be here!
                    return Err(Error::Protocol {
                        op: Operation::GetMessages,
                        msg: String::from(line),
                        back: Backtrace::new(),
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
