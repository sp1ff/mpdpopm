// Copyright (C) 2020 Michael Herstine <sp1ff@pobox.com>
//
// This file is part of mpdpopm.
//
// mpdpopm is free software: you can redistribute it and/or modify it under the terms of the GNU General
// Public License as published by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// mpdpopm is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the
// implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General
// Public License for more details.
//
// You should have received a copy of the GNU General Public License along with mpdpopm.  If not, see
// <http://www.gnu.org/licenses/>.

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
//!    - ratings: "rate RATING TRACK"
//!    - send-to-playlist: "send TRACK PLAYLIST"

#![recursion_limit = "512"] // for the `select!' macro

pub mod clients;
pub mod commands;
pub mod playcounts;
pub mod ratings;
pub mod vars;

use boolinator::Boolinator;
use clients::{Client, IdleClient, IdleSubSystem, PlayerStatus};
use playcounts::{handle_setlp, handle_setpc, PlayState};
use ratings::handle_rating;

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
}

// TODO(sp1ff): re-factor this into one place
macro_rules! error_from {
    ($t:ty) => {
        impl std::convert::From<$t> for Error {
            fn from(err: $t) -> Self {
                Error::Other {
                    cause: Box::new(err),
                    back: Backtrace::generate(),
                }
            }
        }
    };
}

error_from!(commands::Error);
error_from!(clients::Error);
error_from!(playcounts::Error);
error_from!(ratings::Error);
error_from!(std::num::ParseIntError);
error_from!(std::time::SystemTimeError);

pub type Result<T> = std::result::Result<T, Error>;

// TODO(sp1ff): move this to one location
pub type PinnedCmdFut = std::pin::Pin<
    Box<dyn futures::future::Future<Output = tokio::io::Result<std::process::Output>>>,
>;

////////////////////////////////////////////////////////////////////////////////////////////////////

// TODO(sp1ff): Implement reading this from file; how to setup defaults, again?
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    /// Location of log file
    pub log: PathBuf,
    // TODO(sp1ff): I think we need to run this on the same host-- use socket only?
    /// Host on which `mpd' is listening
    host: String,
    /// TCP port on which `mpd' is listening
    port: u16,
    // TODO(sp1ff): If I'm co-located, I can get this directly from the mpd daemon
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
}

impl Config {
    pub fn new() -> Config {
        // TODO(sp1ff): change these defaults to something more appropriate
        Config {
            log: PathBuf::from("/tmp/mpdpopm.log"),
            host: String::from("localhost"),
            port: 16600,
            local_music_dir: PathBuf::from("/mnt/Took-Hall/mp3"),
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
        }
    }
}

// TODO(sp1ff): FuturesUnordered implements Extend; might be nice to write this interface in terms
// of that.
async fn check_messages<I1, I2>(
    client: &mut Client,
    idle_client: &mut IdleClient,
    state: &mut PlayState,
    command_chan: &str,
    rating_sticker: &str,
    ratings_cmd: &str,
    ratings_cmd_args: &mut I1,
    playcount_sticker: &str,
    playcount_cmd: &str,
    playcount_cmd_args: &mut I2,
    lastplayed_sticker: &str,
    music_dir: &str,
    cmds: &mut FuturesUnordered<PinnedCmdFut>,
) -> Result<()>
where
    I1: Iterator<Item = String>,
    I2: Iterator<Item = String>,
{
    let m = idle_client.get_messages().await?;
    for (chan, msgs) in &m {
        // Only supporting a single channel, ATM
        (chan == command_chan).as_option().context(UnknownChannel {
            chan: String::from(chan),
        })?;
        for msg in msgs {
            if msg.starts_with("rate ") {
                match handle_rating(
                    &msg[5..],
                    client,
                    &rating_sticker,
                    &ratings_cmd,
                    ratings_cmd_args,
                    &music_dir,
                    &state.last_status(),
                )
                .await
                {
                    Ok(opt) => match opt {
                        Some(fut) => cmds.push(fut),
                        None => {}
                    },
                    Err(err) => warn!("{:#?}", err),
                }
            } else if msg.starts_with("send ") {
                match state.last_status() {
                    PlayerStatus::Stopped => {
                        warn!("Player is stopped-- can't send the current track to a playlist.");
                    }
                    PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                        client
                            .send_to_playlist(
                                curr.file.to_str().context(BadPath {
                                    pth: curr.file.clone(),
                                })?,
                                &msg,
                            )
                            .await?;
                    }
                }
            } else if msg.starts_with("setpc ") {
                match handle_setpc(
                    &msg[6..],
                    client,
                    &playcount_sticker,
                    &playcount_cmd,
                    playcount_cmd_args,
                    &music_dir,
                    &state.last_status(),
                )
                .await
                {
                    Ok(opt) => match opt {
                        Some(fut) => cmds.push(fut),
                        None => {}
                    },
                    Err(err) => warn!("{:#?}", err),
                }
            } else if msg.starts_with("setlp ") {
                handle_setlp(&msg[6..], client, &lastplayed_sticker, &state.last_status()).await?
            } else {
                return Err(Error::UnknownCommand {
                    msg: String::from(msg),
                    back: Backtrace::generate(),
                });
            }
        }
    } // End `for'.

    Ok(())
}

pub async fn mpdpopm(cfg: Config) -> std::result::Result<(), Error> {
    info!("mpdpopm {} beginning.", vars::VERSION);

    let music_dir = cfg.local_music_dir.to_str().context(BadPath {
        pth: cfg.local_music_dir.clone(),
    })?;

    // TODO(sp1ff): Ugh
    let poll_interval_ms = cfg.poll_interval_ms;

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();

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

    let tick = delay_for(Duration::from_millis(poll_interval_ms)).fuse();

    let ctrl_c = signal::ctrl_c().fuse();
    let sighup = hup.recv().fuse();
    let sigkill = kill.recv().fuse();

    pin_mut!(ctrl_c, sighup, sigkill, tick);

    let mut cmds = FuturesUnordered::<PinnedCmdFut>::new();
    cmds.push(Box::pin(tokio::process::Command::new("pwd").output()));

    let mut done = false;
    while !done {
        debug!("selecting...");
        let mut msg_check_needed = false;
        {
            let mut idle = Box::pin(idle_client.idle().fuse()); // `idle_client' mutably borrowed here
            loop {
                select! {
                    _ = ctrl_c => {
                        info!("got ctrl-C");
                        done = true;
                        break;
                    },
                    _ = sighup => {
                        info!("got SIGHUP");
                        done= true;
                        break;
                    },
                    _ = sigkill => {
                        info!("got SIGKILL");
                        done = true;
                        break;
                    },
                    next = cmds.next() => {
                        // tokio::io::Result<std::process::Output>
                        match next {
                            Some(res) => {
                                // TODO(sp1ff): implement me!
                                debug!("output status is {:#?}", res);
                            },
                            None => {
                                debug!("No more commands to process.");
                            }
                        }
                    },
                    _ = tick => {
                        tick.set(delay_for(Duration::from_millis(poll_interval_ms)).fuse());
                        if let Some(fut) = state.update(&mut client,
                                                        &cfg.playcount_command,
                                                        &mut cfg.playcount_command_args.iter().cloned(),
                                                        music_dir).await? {
                            cmds.push(fut);
                        }
                    },
                    res = idle => {
                        match res {
                            Ok(subsys) => {
                                debug!("subsystem {} changed", subsys);
                                if subsys == IdleSubSystem::Player {
                                    if let Some(fut) = state.update(&mut client,
                                                                    &cfg.playcount_command,
                                                                    &mut cfg.playcount_command_args.iter().cloned(),
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
            }
        } // `idle_client' mutable borrowed dropped here, which is important...

        // because it's mutably borrowed again here:
        if msg_check_needed {
            check_messages(
                &mut client,
                &mut idle_client,
                &mut state,
                &cfg.commands_chan,
                &cfg.rating_sticker,
                &cfg.ratings_command,
                &mut cfg.ratings_command_args.iter().cloned(),
                &cfg.playcount_sticker,
                &cfg.playcount_command,
                &mut cfg.playcount_command_args.iter().cloned(),
                &cfg.lastplayed_sticker,
                music_dir,
                &mut cmds,
            )
            .await?;
        }
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}
