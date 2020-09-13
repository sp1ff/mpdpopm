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

//! # mpdpopm
//!
//! Maintain ratings & playcounts for your mpd server.
//!
//! # Introduction
//!
//! This is a companion daemon for [mpd](https://www.musicpd.org/) that maintains play counts &
//! ratings. Similar to [mpdfav](https://github.com/vincent-petithory/mpdfav), but written in Rust
//! (which I prefer to Go), it will allow you to maintain that information in your tags, as well as
//! the sticker database, by invoking external commands to keep your tags up-to-date (something
//! along the lines of [mpdcron](https://alip.github.io/mpdcron)).
//!
//! # Commands
//!
//! I'm currently sending all commands over one channel:
//!
//!    - ratings: "rate RATING( TRACK)?"
//!    - set playcount: "setpc PC( TRACK)?"
//!    - set lastplayed: "setlp TIMEESTAMP( TRACK)?"
//!    - send-to-playlist: "send PLAYLIST" (sends the current track only)
//!
//! If compiled with the 'scribbu' feature enabled:
//!
//!    - set XTAG: "setxtag XTAG MERGE( TRACK)?"
//!    - set the genre: "setgenre WINAMP-GENRE-NUMBER( TRACK)?"
//!

#![recursion_limit = "512"] // for the `select!' macro

pub mod clients;
pub mod commands;
pub mod error_from;
pub mod playcounts;
pub mod ratings;
#[cfg(feature = "scribbu")]
pub mod scribbu;
pub mod vars;

use boolinator::Boolinator;
use clients::{Client, IdleClient, IdleSubSystem, PlayerStatus};
use commands::TaggedCommandFuture;
use playcounts::{set_last_played, set_play_count, PlayState};
use ratings::{set_rating, RatedTrack, RatingRequest};
use vars::{LOCALSTATEDIR, PREFIX};

use futures::{
    future::FutureExt,
    pin_mut, select,
    stream::{FuturesUnordered, StreamExt},
};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};
use tokio::{
    signal,
    signal::unix::{signal, SignalKind},
    time::{delay_for, Duration},
};

use std::convert::TryFrom;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("The path `{}' cannot be converted to a UTF-8 string", pth.display()))]
    BadPath { pth: PathBuf },
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
    #[snafu(display("We received an unknown message: `{}'", msg))]
    UnknownCommand {
        msg: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Can't rate the current track when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("`{}' is not implemented, yet", feature))]
    NotImplemented { feature: String },
}

error_from!(commands::Error);
error_from!(clients::Error);
error_from!(playcounts::Error);
error_from!(ratings::Error);
#[cfg(feature = "scribbu")]
error_from!(scribbu::Error);
error_from!(std::num::ParseIntError);
error_from!(std::time::SystemTimeError);

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

// Not behind a feature flag so that a non-scribbu build can still deserialize scribbu-config
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct ScribbuConfig {
    /// Command, with replacement parameters, to be run to set a song's genre
    genre_command: String,
    /// Args, with replacement parameters, for the genrecommand
    genre_command_args: Vec<String>,
}

impl Default for ScribbuConfig {
    fn default() -> ScribbuConfig {
        ScribbuConfig {
            genre_command: String::new(),
            genre_command_args: Vec::<String>::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Location of log file
    pub log: PathBuf,
    /// Host on which `mpd' is listening
    host: String,
    /// TCP port on which `mpd' is listening
    port: u16,
    /// The `mpd' root music directory, relative to the host on which *this* daemon is running
    local_music_dir: PathBuf,
    /// Sticker name under which to store playcounts
    playcount_sticker: String,
    /// Sticker name under which to store the last played timestamp
    lastplayed_sticker: String,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    played_thresh: f64,
    /// The interval, in milliseconds, at which to poll `mpd' for the current state
    poll_interval_ms: u64,
    /// Channel to setup for assorted commands-- channel names must satisfy "[-a-zA-Z-9_.:]+"
    commands_chan: String,
    /// Command, with replacement parameters, to be run when a song's playcount is incremented
    playcount_command: String,
    /// Args, with replacement parameters, for the playcount command
    playcount_command_args: Vec<String>,
    /// Sticker under which to store song ratings, as a textual representation of a number in
    /// [0,255]
    rating_sticker: String,
    /// Command, with replacement parameters, to be run when a song is rated
    ratings_command: String,
    /// Args, with replacement parameters, for the ratings command
    ratings_command_args: Vec<String>,
    /// (optional) scribbu configuration
    scribbu: ScribbuConfig,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            log: [LOCALSTATEDIR, "log", "mppopmd.log"].iter().collect(),
            host: String::from("localhost"),
            port: 6600,
            local_music_dir: [PREFIX, "Music"].iter().collect(),
            playcount_sticker: String::from("unwoundstack.com:playcount"),
            lastplayed_sticker: String::from("unwoundstack.com:lastplayed"),
            played_thresh: 0.6,
            poll_interval_ms: 5000,
            commands_chan: String::from("unwoundstack.com:commands"),
            playcount_command: String::new(),
            playcount_command_args: Vec::<String>::new(),
            rating_sticker: String::from("unwoundstack.com:rating"),
            ratings_command: String::new(),
            ratings_command_args: Vec::<String>::new(),
            scribbu: ScribbuConfig::default(),
        }
    }
}

/// Collective state needed for processing messages
pub struct MessagesContext<'a, I1, I2>
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
}

impl<I1, I2> MessagesContext<'_, I1, I2>
where
    I1: Iterator<Item = String> + Clone,
    I2: Iterator<Item = String> + Clone,
{
    /// Whip up a new instance; other than cloning the iterators, should just hold references in the
    /// enclosing scope
    pub fn new<'a>(
        music_dir: &'a str,
        rating_sticker: &'a str,
        ratings_cmd: &'a str,
        ratings_cmd_args: I1,
        playcount_sticker: &'a str,
        playcount_cmd: &'a str,
        playcount_cmd_args: I2,
        lastplayed_sticker: &'a str,
    ) -> MessagesContext<'a, I1, I2> {
        MessagesContext {
            music_dir: music_dir,
            rating_sticker: rating_sticker,
            ratings_cmd: ratings_cmd,
            ratings_cmd_args: ratings_cmd_args.clone(),
            playcount_sticker: playcount_sticker,
            playcount_cmd: playcount_cmd,
            playcount_cmd_args: playcount_cmd_args.clone(),
            lastplayed_sticker: lastplayed_sticker,
        }
    }
    pub async fn process(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
        if msg.starts_with("rate ") {
            self.rate(&msg[5..], client, state).await
        } else if msg.starts_with("send ") {
            self.send(&msg[5..], client, state).await
        } else if msg.starts_with("setpc ") {
            self.setpc(&msg[6..], client, state).await
        } else if msg.starts_with("setlp ") {
            self.setlp(&msg[6..], client, state).await
        } else {
            self.maybe_handle_scribbu(&msg, client, state).await
        }
    }
    /// Handle rating message: "RATING( TRACK)?"
    async fn rate(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
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
    /// Handle send-to-playlist: "SEND PLAYLIST"
    async fn send(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
        match state {
            PlayerStatus::Stopped => {
                warn!("Player is stopped-- can't send the current track to a playlist.");
                Ok(None)
            }
            PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                client
                    .send_to_playlist(
                        curr.file.to_str().context(BadPath {
                            pth: curr.file.clone(),
                        })?,
                        msg,
                    )
                    .await?;
                Ok(None)
            }
        }
    }
    /// Handle `setpc': "PC( TRACK)?"
    async fn setpc(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
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
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
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
    #[cfg(feature = "scribbu")]
    async fn maybe_handle_scribbu(
        &self,
        msg: &str,
        client: &mut Client,
        state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
        use scribbu::{set_genre, set_xtag};
        if msg.starts_with("setgenre ") {
            Ok(set_genre(&msg[9..], client, state, self.music_dir)?)
        } else if msg.starts_with("setxtag ") {
            Ok(set_xtag(&msg[8..], client, state, self.music_dir)?)
        } else {
            Err(Error::UnknownCommand {
                msg: String::from(msg),
                back: Backtrace::generate(),
            })
        }
    }
    #[cfg(not(feature = "scribbu"))]
    async fn maybe_handle_scribbu(
        &self,
        msg: &str,
        _client: &mut Client,
        _state: &PlayerStatus,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
        log::error!(
            "Message `{}' received, but compiled without scribbu support; enable feature \
\"scribbu\" to handle this message.",
            msg
        );
        Ok(None)
    }
}

async fn check_messages<'a, I1, I2, E>(
    client: &mut Client,
    idle_client: &mut IdleClient,
    state: PlayerStatus,
    command_chan: &str,
    ctx: &MessagesContext<'a, I1, I2>,
    cmds: &mut E,
) -> Result<()>
where
    I1: Iterator<Item = String> + Clone,
    I2: Iterator<Item = String> + Clone,
    E: Extend<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>,
{
    let m = idle_client.get_messages().await?;
    for (chan, msgs) in &m {
        // Only supporting a single channel, ATM
        (chan == command_chan).as_option().context(UnknownChannel {
            chan: String::from(chan),
        })?;
        for msg in msgs {
            cmds.extend(ctx.process(msg, client, &state).await?);
        }
    }

    Ok(())
}

/// Core `mppopmd' logic
pub async fn mpdpopm(cfg: Config) -> std::result::Result<(), Error> {
    info!("mpdpopm {} beginning.", vars::VERSION);

    // We need the music directory to be convertible to string; check that first-off:
    let music_dir = cfg.local_music_dir.to_str().context(BadPath {
        pth: cfg.local_music_dir.clone(),
    })?;

    let mut client = Client::connect(format!("{}:{}", cfg.host, cfg.port)).await?;
    let mut state = PlayState::new(
        &mut client,
        &cfg.playcount_sticker,
        &cfg.lastplayed_sticker,
        cfg.played_thresh,
    )
    .await?;

    let mut idle_client = IdleClient::connect(format!("{}:{}", cfg.host, cfg.port)).await?;
    idle_client.subscribe(&cfg.commands_chan).await?;

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();
    let ctrl_c = signal::ctrl_c().fuse();

    let sighup = hup.recv().fuse();
    let sigkill = kill.recv().fuse();

    let tick = delay_for(Duration::from_millis(cfg.poll_interval_ms)).fuse();
    pin_mut!(ctrl_c, sighup, sigkill, tick);

    let mut cmds = FuturesUnordered::<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>::new();
    cmds.push(TaggedCommandFuture::pin(
        Box::pin(tokio::process::Command::new("pwd").output()),
        None,
    ));

    let ctx = MessagesContext::new(
        &music_dir,
        &cfg.rating_sticker,
        &cfg.ratings_command,
        cfg.ratings_command_args.iter().cloned(),
        &cfg.playcount_sticker,
        &cfg.playcount_command,
        cfg.playcount_command_args.iter().cloned(),
        &cfg.lastplayed_sticker,
    );

    let mut done = false;
    while !done {
        debug!("selecting...");
        let mut msg_check_needed = false;
        {
            // `idle_client' mutably borrowed here
            let mut idle = Box::pin(idle_client.idle().fuse());
            loop {
                select! {
                    _ = ctrl_c => {
                        info!("got ctrl-C");
                        done = true;
                        break;
                    },
                    _ = sighup => {
                        info!("got SIGHUP");
                        done = true;
                        break;
                    },
                    _ = sigkill => {
                        info!("got SIGKILL");
                        done = true;
                        break;
                    },
                    _ = tick => {
                        tick.set(delay_for(Duration::from_millis(cfg.poll_interval_ms)).fuse());
                        if let Some(fut) = state.update(&mut client,
                                                        &cfg.playcount_command,
                                                        &mut cfg.playcount_command_args
                                                        .iter()
                                                        .cloned(),
                                                        music_dir).await? {
                            cmds.push(fut);
                        }
                    },
                    next = cmds.next() => match next {
                        Some(out) => {
                            debug!("output status is {:#?}", out.out);
                            match out.upd {
                                Some(uri) => {
                                    debug!("{} needs to be updated", uri);
                                    client.update(&uri).await?;
                                },
                                None => debug!("No database update needed"),
                            }
                        },
                        None => {
                            debug!("No more commands to process.");
                        }
                    },
                    res = idle => match res {
                        Ok(subsys) => {
                            debug!("subsystem {} changed", subsys);
                            if subsys == IdleSubSystem::Player {
                                if let Some(fut) = state.update(&mut client,
                                                                &cfg.playcount_command,
                                                                &mut cfg.playcount_command_args
                                                                .iter()
                                                                .cloned(),
                                                                music_dir).await? {
                                    cmds.push(fut);
                                }
                            } else if subsys == IdleSubSystem::Message {
                                msg_check_needed = true;
                            }
                            break;
                        },
                        Err(err) => {
                            debug!("error {} on idle", err);
                            done = true;
                            break;
                        }
                    }
                }
            }
        } // `idle_client' mutable borrowed dropped here, which is important...

        // because it's mutably borrowed again here:
        if msg_check_needed {
            check_messages(
                &mut client,
                &mut idle_client,
                state.last_status(),
                &cfg.commands_chan,
                &ctx,
                &mut cmds,
            )
            .await?;
        }
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}
