//! MPD clients and associated utilities.
//!
//! # Introduction
//!
//! This module contains basic types implementing various `mpd' client operations. Cf. the [MPD
//! protocol](http://www.musicpd.org/doc/protocol/).

use crate::ReplacementStringError;

use log::{debug, info};
use regex::Regex;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::prelude::*;

use std::collections::HashMap;
use std::fmt;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                               Error                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An enumeration of `mpd' client errors
#[derive(Debug)]
pub enum Cause {
    /// An error occurred in some sub-system (network I/O, or string formatting, e.g.)
    SubModule,
    /// Error upon connecting to the mpd server; includes the text returned (if any)
    Connect(String),
    /// Error in regex matching on mpd output
    Regex(String),
    /// Error in respone to "readmessages"
    GetMessages(String),
    /// Error in respone to "sticker set"
    StickerSet(String),
    /// Error in respone to "subscribe"
    Subscribe(String),
    /// Erorr in respone to "idle"
    Idle(String),
    /// Unknown idle subsystem
    IdleSubsystem(String),
}

/// An Error that occurred when dealing with the mpd server.

/// This could be a direct result of an "ACK" line, or when trying to interpret the mpd server's
/// output. In the former case, [`Error::source`] will be [`None`]. In the latter, it will contain
/// the original [`std::error::Error`].
///
/// [`None`]: std::option::Option::None
#[derive(Debug)]
pub struct Error {
    /// Enumerated status code-- perhaps this is a holdover from my C++ days, but I've found that
    /// programmatic error-handling is facilitated by status codes, not text. Textual messages
    /// should be synthesized from other information only when it is time to present the error to a
    /// human (in a log file, say).
    cause: Cause,
    // This is an Option that may contain a Box containing something that implements
    // std::error::Error.  It is still unclear to me how this satisfies the lifetime bound in
    // std::error::Error::source, which additionally mandates that the boxed thing have 'static
    // lifetime. There is a discussion of this at
    // <https://users.rust-lang.org/t/what-does-it-mean-to-return-dyn-error-static/37619/6>,
    // but at the time of this writing, i cannot follow it.
    source: Option<Box<dyn std::error::Error>>,
}

impl Error {
    /// Create a new Error instance at the "protocol level"; i.e. when something's gone wrong in our
    /// interpretation of the mpd protocol. If an error took place in some subordinate function call
    /// (socket I/O, for instance), the conversion will be handled by a From implementation.
    pub fn new(cause: Cause) -> Error {
        Error {
            cause: cause,
            source: None,
        }
    }
}

impl fmt::Display for Error {
    /// Format trait for an empty format, {}.
    ///
    /// I'm unsure of the conventions around implementing this method, but I've chosen to provide
    /// a one-line representation of the error suitable for writing to a log file or stderr.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.cause {
            Cause::SubModule => write!(f, "Cause: {:#?}", self.source),
            Cause::Connect(x) => write!(f, "Connect error: {}", x),
            Cause::Regex(x) => write!(f, "Error in regex matching on mpd output {}", x),
            Cause::GetMessages(x) => write!(f, "Error in respone to readmessages: {}", x),
            Cause::StickerSet(x) => write!(f, "While setting sticker: {}", x),
            Cause::Subscribe(x) => write!(f, "While subscribing: {}", x),
            Cause::Idle(x) => write!(f, "In response to idle: {}", x),
            Cause::IdleSubsystem(x) => write!(f, "Unknown idle subsystem {}", x),
        }
        // let line = match self.cause {
        //     Cause::Connect(x) => format!("Connect error: {}.", x),
        // };
        // write!(f, "{}", line)
    }
}

impl std::error::Error for Error {
    /// The lower-level source of this error, if any.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            // This is an Option that may contain a reference to something that implements
            // std::error::Error and has lifetime 'static. I'm still not sure what 'static means,
            // exactly, but at the time of this writing, I take it to mean a thing which can, if
            // needed, last for the program lifetime (e.g. it contains no references to anything
            // that itself does not have 'static lifetime)
            Some(bx) => Some(bx.as_ref()),
            None => None,
        }
    }
}

// There *must* be a way to do this generically, but my naive attempts clash with the core
// implementation of From<T> for T.

impl std::convert::From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<std::num::ParseIntError> for Error {
    fn from(err: std::num::ParseIntError) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<std::num::ParseFloatError> for Error {
    fn from(err: std::num::ParseFloatError) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<ReplacementStringError> for Error {
    fn from(err: ReplacementStringError) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<regex::Error> for Error {
    fn from(err: regex::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

// TODO(sp1ff): see about making these elements private
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

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           Utilities                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Core logic for connecting to an mpd server; for private use.
async fn connect<A: ToSocketAddrs>(addr: A) -> Result<TcpStream> {
    let mut sock = TcpStream::connect(addr).await?;

    let mut buf = Vec::with_capacity(32);
    let _cb = sock.read_buf(&mut buf).await?;

    let text = String::from_utf8(buf)?;
    if !text.starts_with("OK MPD ") {
        return Err(Error::new(Cause::Connect(text.trim().to_string())));
    }

    info!("Connected {}.", text[7..].trim());

    Ok(sock)
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                               Client                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// General-purpose mpdpopm client abstraction.

/// I should probably make this generic over all types implementing AsyncRead & AsyncWrite.
#[derive(Debug)]
pub struct Client {
    sock: TcpStream,
}

impl Client {
    /// Create a new [`mpdpopm::client::`][`Client`] instance from something that implements
    /// [`ToSocketAddrs`][`tokio::net::ToSocketAddrs`]
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Client> {
        let sock = connect(addr).await?;
        Ok(Client { sock: sock })
    }

    /// Retrieve the current server status.
    pub async fn status(&mut self) -> Result<PlayerStatus> {
        self.sock.write_all(b"status\n").await?;

        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;
        let text = String::from_utf8(buf)?;

        // When the server is playing or paused, will look something like this:

        // volume: -1
        // repeat: 0
        // random: 0
        // single: 0
        // consume: 0
        // playlist: 3
        // playlistlength: 87
        // mixrampdb: 0.000000
        // state: play
        // song: 14
        // songid: 15
        // time: 141:250
        // bitrate: 128
        // audio: 44100:24:2
        // nextsong: 15
        // nextsongid: 16
        // elapsed: 140.585

        // but if the state is "stop", much of that will be missing; it will look more like:

        // volume: -1
        // repeat: 0
        // random: 0
        // single: 0
        // consume: 0
        // playlist: 84
        // playlistlength: 27
        // mixrampdb: 0.000000
        // state: stop

        // I first thought to avoid the use (and cost) of regular expressoins by just doing
        // sub-string searching on "state: ", but when I realized I needed to only match
        // at the beginning of a line bailed & just went ahead.

        // TODO(sp1ff): Ugh-- this all seems way too prolix.

        let re = Regex::new(r"(?m)^state: (play|pause|stop)$")?;
        let caps = re
            .captures(&text)
            .ok_or(Error::new(Cause::Regex(text.to_string())))?;
        let state = caps
            .get(1)
            .ok_or(Error::new(Cause::Regex(text.to_string())))?
            .as_str();

        match state {
            "stop" => Ok(PlayerStatus::Stopped),
            "play" | "pause" => {
                let re = Regex::new(r"(?m)^songid: ([0-9]+)$")?;
                let caps = re
                    .captures(&text)
                    .ok_or(Error::new(Cause::Regex(text.to_string())))?;
                let songid = caps
                    .get(1)
                    .ok_or(Error::new(Cause::Regex(text.to_string())))?
                    .as_str()
                    .parse::<u64>()?;

                let re = Regex::new(r"(?m)^elapsed: ([.0-9]+)$")?;
                let caps = re
                    .captures(&text)
                    .ok_or(Error::new(Cause::Regex(text.to_string())))?;
                let elapsed = caps
                    .get(1)
                    .ok_or(Error::new(Cause::Regex(text.to_string())))?
                    .as_str()
                    .parse::<f64>()?;

                // navigate from `songid'-- don't send a "currentsong" message-- the current song
                // could have changed
                self.sock
                    .write_all(format!("playlistid {}\n", songid).as_bytes())
                    .await?;

                let mut buf2 = Vec::with_capacity(512);
                self.sock.read_buf(&mut buf2).await?;

                let text2 = String::from_utf8(buf2)?;

                // Should look something like this:
                // file: U-Z/U2 - Who's Gonna RIDE Your WILD HORSES.mp3
                // Last-Modified: 2004-12-24T19:26:13Z
                // Artist: U2
                // Title: Who's Gonna RIDE Your WILD HOR
                // Genre: Pop
                // Time: 316
                // Pos: 41
                // Id: 42
                // duration: 249.994

                let re = Regex::new(r"(?m)^file: (.*)$")?;
                let caps = re
                    .captures(&text2)
                    .ok_or(Error::new(Cause::Regex(text2.to_string())))?;
                let file = std::path::PathBuf::from(
                    caps.get(1)
                        .ok_or(Error::new(Cause::Regex(text2.to_string())))?
                        .as_str(),
                );

                let re = Regex::new(r"(?m)^duration: (.*)$")?;
                let caps = re
                    .captures(&text2)
                    .ok_or(Error::new(Cause::Regex(text2.to_string())))?;
                let duration = caps
                    .get(1)
                    .ok_or(Error::new(Cause::Regex(text2.to_string())))?
                    .as_str()
                    .parse::<f64>()?;

                let curr = CurrentSong::new(songid, file, elapsed, duration);

                match state {
                    "play" => Ok(PlayerStatus::Play(curr)),
                    "pause" => Ok(PlayerStatus::Pause(curr)),
                    // TODO(sp1ff): handle this better
                    _ => panic!("unknown state"),
                }
            }
            // TODO(sp1ff): handle this properly
            _ => panic!("unknown state"),
        }
    }

    // TODO(sp1ff): What's the idiomatic way to allow callers to request that we coerce the
    // sticker value (if present) to the expected type?
    /// Retrieve a song sticker by name, as a string
    pub async fn get_sticker(&mut self, file: &str, sticker_name: &str) -> Result<Option<String>> {
        self.sock
            .write_all(format!("sticker get song \"{}\" \"{}\"\n", file, sticker_name).as_bytes())
            .await?;
        debug!("Sent sticker get message.");

        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        debug!("From sticker get, got '{}'.", text);

        let mut val: Option<String> = None;
        let prefix = format!("sticker: {}=", sticker_name);
        for line in text.lines() {
            if line.starts_with(&prefix) {
                val = Some(line[prefix.len()..].to_string());
            }
        }
        match val {
            Some(val) => Ok(Some(val)),
            None => Ok(None),
        }
    }

    // TODO(sp1ff): What's the idiomatic way to allow callers to pass the sticker value by any
    // type?
    /// Set a song sticker by name, as text
    pub async fn set_sticker(
        &mut self,
        file: &str,
        sticker_name: &str,
        sticker_value: &str,
    ) -> Result<()> {
        self.sock
            .write_all(
                format!(
                    "sticker set song \"{}\" \"{}\" \"{}\"\n",
                    file, sticker_name, sticker_value
                )
                .as_bytes(),
            )
            .await?;
        debug!("Sent sticker set message.");

        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        debug!("From sticker set, got '{}'.", text);

        for line in text.lines() {
            if line.starts_with("ACK") {
                return Err(Error::new(Cause::StickerSet(String::from(line))));
            }
        }
        Ok(())
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

impl IdleSubSystem {
    fn from_string(text: &str) -> Result<IdleSubSystem> {
        let x = text.to_lowercase();
        if x == "player" {
            Ok(IdleSubSystem::Player)
        } else if x == "message" {
            Ok(IdleSubSystem::Message)
        } else {
            Err(Error::new(Cause::IdleSubsystem(String::from(text))))
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
#[derive(Debug)]
pub struct IdleClient {
    sock: TcpStream,
}

impl IdleClient {
    /// Create a new [`mpdpopm::client::IdleClient`][`IdleClient`] instance from something that
    /// implements [`ToSocketAddrs`][`tokio::net::ToSocketAddrs`]
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<IdleClient> {
        let sock = connect(addr).await?;
        Ok(IdleClient { sock: sock })
    }

    /// Subscribe to an mpd channel
    pub async fn subscribe(&mut self, chan: &str) -> Result<()> {
        self.sock
            .write_all(format!("subscribe {}\n", chan).as_bytes())
            .await?;
        debug!("Sent subscribe message for {}.", chan);

        let mut buf = Vec::with_capacity(32);
        let _cb = self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        if !text.starts_with("OK") {
            return Err(Error::new(Cause::Subscribe(String::from(text))));
        }

        debug!("Subscribed to {}.", chan);

        Ok(())
    }

    /// Enter idle state-- return the subsystem that changed, causing the connection to return
    pub async fn idle(&mut self) -> Result<IdleSubSystem> {
        self.sock.write_all(b"idle player message\n").await?;
        debug!("Sent idle message.");

        let mut buf = Vec::with_capacity(32);
        self.sock.read_buf(&mut buf).await?;

        // If the player state changes, we'll get: "changed: player\nOK\n"

        // If a ratings message is sent, we'll get: "changed: message\nOK\n", to which we respond
        // "readmessages", which should give us something like:
        //
        //     channel: ratings
        //     message: 255
        //     OK
        //
        // We remain subscribed, but we need to send a new idle message.
        let mut text = String::from_utf8(buf)?;
        if !text.starts_with("changed: ") {
            return Err(Error::new(Cause::Idle(String::from(text))));
        }

        let idx = text.find('\n').unwrap();
        let result = IdleSubSystem::from_string(&text[9..idx])?;
        text = text[idx + 1..].to_string();
        if !text.starts_with("OK") {
            return Err(Error::new(Cause::Idle(String::from(text))));
        }

        Ok(result)
    }

    /// This method simply returns the results of a "readmessages" as a HashMap of channel name to
    /// Vec of (String) messages for that channel
    pub async fn get_messages(&mut self) -> Result<HashMap<String, Vec<String>>> {
        self.sock.write_all(b"readmessages\n").await?;
        debug!("Sent readmessages.");

        // We expect something like:
        //
        //     channel: ratings
        //     message: 255
        //     OK
        //
        // We remain subscribed, but we need to send a new idle message.
        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        debug!("From readmessages, got '{}'.", text);

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
                    if !line.starts_with("channel: ") {
                        return Err(Error::new(Cause::GetMessages(String::from(line))));
                    }
                    chan = String::from(&line[9..]);
                    state = State::Running;
                }
                State::Running => {
                    if line.starts_with("message: ") {
                        msgs.push(String::from(&line[9..]));
                    } else if line.starts_with("channel: ") {
                        m.insert(chan.clone(), msgs.clone());
                        chan = String::from(&line[9..]);
                        msgs = Vec::new();
                    } else if line == "OK" {
                        m.insert(chan.clone(), msgs.clone());
                        state = State::Finished;
                    } else {
                        return Err(Error::new(Cause::GetMessages(String::from(line))));
                    }
                }
                State::Finished => {
                    // Should never be here!
                    return Err(Error::new(Cause::GetMessages(String::from(line))));
                }
            }
        }

        Ok(m)
    }
}
