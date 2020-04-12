//! MPD clients and associated utilities.

//! Cf. the [MPD protocol][proto].
//!
//! [proto]: http://www.musicpd.org/doc/protocol/

use crate::ReplacementStringError;

use log::{debug, info};
use std::error::Error;
use std::fmt;
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::prelude::*;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         MpdClientError                                         //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An Error when dealing with the mpd server.

/// This could be a direct result of an "ACK" line, or when trying to interpret the mpd server's
/// output.
#[derive(Debug, Clone)]
pub struct MpdClientError {
    details: String,
}

impl MpdClientError {
    pub fn new(msg: &str) -> MpdClientError {
        MpdClientError {
            details: msg.to_string(),
        }
    }
}

impl fmt::Display for MpdClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for MpdClientError {
    fn description(&self) -> &str {
        &self.details
    }
}

impl From<std::io::Error> for MpdClientError {
    fn from(err: std::io::Error) -> Self {
        MpdClientError {
            details: format!("{:#?}", err),
        }
    }
}

impl From<std::string::FromUtf8Error> for MpdClientError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        MpdClientError {
            details: format!("{:#?}", err),
        }
    }
}

impl From<std::num::ParseIntError> for MpdClientError {
    fn from(err: std::num::ParseIntError) -> Self {
        MpdClientError {
            details: format!("failed to parse an integer: {:#?}", err),
        }
    }
}

impl From<std::num::ParseFloatError> for MpdClientError {
    fn from(err: std::num::ParseFloatError) -> Self {
        MpdClientError {
            details: format!("failed to parse a floating-point number: {:#?}", err),
        }
    }
}

// TODO(sp1ff): want this?
impl From<ReplacementStringError> for MpdClientError {
    fn from(err: ReplacementStringError) -> Self {
        MpdClientError {
            details: format!("bad replacement string: {:#?}", err),
        }
    }
}

/// Core logic for connecting to an mpd server; for private use.
async fn connect<A: ToSocketAddrs>(addr: A) -> Result<TcpStream, MpdClientError> {
    let mut sock = TcpStream::connect(addr).await?;

    let mut buf = Vec::with_capacity(32);
    let _cb = sock.read_buf(&mut buf).await?;

    let text = String::from_utf8(buf)?;
    if !text.starts_with("OK MPD ") {
        return Err(MpdClientError::new(text.trim()));
    }

    info!("Connected {}.", text[7..].trim());

    Ok(sock)
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                        enum PlayerState                                        //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerState {
    Play,
    Pause,
    Stop,
}

impl PlayerState {
    fn from_string(text: &str) -> Result<PlayerState, MpdClientError> {
        let x = text.to_lowercase();
        if x == "play" {
            Ok(PlayerState::Play)
        } else if x == "pause" {
            Ok(PlayerState::Pause)
        } else if x == "stop" {
            Ok(PlayerState::Stop)
        } else {
            Err(MpdClientError::new(&format!(
                "can't convert {} to PlayerState",
                text
            )))
        }
    }
}

/// mpdpopm server status.
#[derive(Clone, Debug)]
pub struct ServerStatus {
    pub state: PlayerState,
    pub songid: u64,
    pub file: std::path::PathBuf,
    pub elapsed: f64,
    pub duration: f64,
}

/// General-purpose mpdpopm client abstraction.

/// I should probably make this generic over all types implementing AsyncRead & AsyncWrite.
#[derive(Debug)]
pub struct Client {
    sock: TcpStream,
}

impl Client {
    /// Create a new [`mpdpopm::client::`][`Client`] instance from something that implements
    /// [`ToSocketAddrs`][`tokio::net::ToSocketAddrs`]
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Client, MpdClientError> {
        let sock = connect(addr).await?;
        Ok(Client { sock: sock })
    }

    /// Retrieve the current server status.
    pub async fn status(&mut self) -> Result<ServerStatus, MpdClientError> {
        self.sock.write_all(b"status\n").await?;

        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;

        // Will look something like this:
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
        let mut text = String::from_utf8(buf)?;

        // TODO(sp1ff): Ugh-- there must be a better way; I *could* initialize each variable
        // by matching a regex, but that seems much less efficient.
        let mut state_opt: Option<PlayerState> = None;
        let mut songid_opt: Option<u64> = None;
        let mut elapsed_opt: Option<f64> = None;
        for line in text.lines() {
            if line.starts_with("state: ") {
                state_opt = Some(PlayerState::from_string(&line[7..])?);
            } else if line.starts_with("songid: ") {
                songid_opt = Some(line[8..].to_string().parse::<u64>()?);
            } else if line.starts_with("elapsed: ") {
                elapsed_opt = Some(line[9..].to_string().parse::<f64>()?);
            }
        }

        // TODO(sp1ff): what if there's no song loaded/no playlist/whatever?
        let state = match state_opt {
            Some(state) => state,
            None => return Err(MpdClientError::new("no player state")),
        };
        let songid = match songid_opt {
            Some(id) => id,
            None => return Err(MpdClientError::new("no songid")),
        };
        let elapsed = match elapsed_opt {
            Some(f) => f,
            None => 0.,
        };

        // navigate from `songid'-- don't send a "currentsong" message-- the current song could have
        // changed
        self.sock
            .write_all(format!("playlistid {}\n", songid).as_bytes())
            .await?;

        let mut buf2 = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf2).await?;

        text = String::from_utf8(buf2)?;

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
        let mut file_opt: Option<std::path::PathBuf> = None;
        let mut duration_opt: Option<f64> = None;
        for line in text.lines() {
            if line.starts_with("file: ") {
                file_opt = Some(std::path::PathBuf::from(&line[6..]));
            } else if line.starts_with("duration: ") {
                duration_opt = Some(line[10..].to_string().parse::<f64>()?);
            }
        }

        let file = match file_opt {
            Some(f) => f,
            None => return Err(MpdClientError::new("no file")),
        };
        let duration = match duration_opt {
            Some(f) => f,
            None => 0.,
        };

        Ok(ServerStatus {
            state: state,
            songid: songid,
            file: file,
            elapsed: elapsed,
            duration: duration,
        })
    }

    pub async fn get_sticker(
        &mut self,
        file: &str,
        sticker_name: &str,
    ) -> Result<Option<String>, MpdClientError> {
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

    pub async fn set_sticker(
        &mut self,
        file: &str,
        sticker_name: &str,
        sticker_value: &str,
    ) -> Result<(), MpdClientError> {
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
                return Err(MpdClientError::new(&text));
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IdleSubSystem {
    Player,
    Message,
}

impl IdleSubSystem {
    fn from_string(text: &str) -> Result<IdleSubSystem, MpdClientError> {
        let x = text.to_lowercase();
        if x == "player" {
            Ok(IdleSubSystem::Player)
        } else if x == "message" {
            Ok(IdleSubSystem::Message)
        } else {
            Err(MpdClientError::new(&format!(
                "can't convert {} to IdleSubSystem",
                text
            )))
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

////////////////////////////////////////////////////////////////////////////////////////////////////

/// mpdpopm client for "idle" connections.

/// I should probably make this generic over all types implementing AsyncRead & AsyncWrite.
#[derive(Debug)]
pub struct IdleClient {
    sock: TcpStream,
}

impl IdleClient {
    /// Create a new [`mpdpopm::client::IdleClient`][`IdleClient`] instance from something that
    /// implements [`ToSocketAddrs`][`tokio::net::ToSocketAddrs`]
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<IdleClient, MpdClientError> {
        let sock = connect(addr).await?;
        Ok(IdleClient { sock: sock })
    }

    /// Subscribe to an mpd channel
    pub async fn subscribe(&mut self, chan: &str) -> Result<(), MpdClientError> {
        self.sock
            .write_all(format!("subscribe {}\n", chan).as_bytes())
            .await?;
        debug!("Sent subscribe message for {}.", chan);

        let mut buf = Vec::with_capacity(32);
        let _cb = self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        if !text.starts_with("OK") {
            return Err(MpdClientError::new(text.trim()));
        }

        debug!("Subscribed to {}.", chan);

        Ok(())
    }

    /// Enter idle state-- return the subsystem that changed, causing the connection to return
    pub async fn idle(&mut self) -> Result<IdleSubSystem, MpdClientError> {
        self.sock.write_all(b"idle player message\n").await?;
        debug!("Sent idle message.");

        let mut buf = Vec::with_capacity(32);
        self.sock.read_buf(&mut buf).await?;

        // If the player state changes, we'll get: "changed: player\nOK\n"

        // If a ratings message is sent, we'll get: "changed: message\nOK\n", to which we respond
        // "readmessages", which should give us something like:
        // "readmessages\nchannel: ratings\nmessage: 255\nOK\n". We remain subscribed, but we need
        // to send a new idle message.
        let mut text = String::from_utf8(buf)?;
        debug!("From idle, got '{}'.", text);
        if !text.starts_with("changed: ") {
            return Err(MpdClientError::new(text.trim()));
        }

        let idx = text.find('\n').unwrap();
        let result = IdleSubSystem::from_string(&text[9..idx])?;
        text = text[idx + 1..].to_string();
        if !text.starts_with("OK") {
            return Err(MpdClientError::new(text.trim()));
        }

        Ok(result)
    }
    pub async fn get_messages(&mut self) -> Result<Vec<String>, MpdClientError> {
        self.sock.write_all(b"readmessages\n").await?;
        debug!("Sent readmessages.");

        let mut buf = Vec::with_capacity(512);
        self.sock.read_buf(&mut buf).await?;

        let text = String::from_utf8(buf)?;
        debug!("From readmessages, got '{}'.", text);

        Ok(text.lines().map(|x| x.to_string()).collect())
    }
}
