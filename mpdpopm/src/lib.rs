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
pub mod playcounts;
pub mod ratings;
pub mod replstrings;
pub mod vars;

use boolinator::Boolinator;
use clients::{Client, IdleClient, IdleSubSystem, PlayerStatus};
use playcounts::PlayState;
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
    BadPath {
        pth: PathBuf
    },
    #[snafu(display("We received messages for an unknown channel `{}'; this is likely a bug; please
consider filing a report to sp1ff@pobox.com", chan))]
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
    ($t:ty) => (
        impl std::convert::From<$t> for Error {
            fn from(err: $t) -> Self {
                Error::Other{ cause: Box::new(err), back: Backtrace::generate() }
            }
        }
    )
}

error_from!(replstrings::Error);
error_from!(clients::Error);
error_from!(playcounts::Error);
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

async fn check_messages(client: &mut Client,
                        idle_client: &mut IdleClient,
                        state: &mut PlayState,
                        command_chan: &str,
                        rating_sticker: &str,
                        ratings_cmd: &str,
                        ratings_cmd_args: &Vec<String>,
                        music_dir: &PathBuf,
                        cmds: &mut FuturesUnordered<PinnedCmdFut>) -> Result<()> {

    let m = idle_client.get_messages().await?;
    for (chan, msgs) in &m {
        // Only supporting a single channel, ATM
        (chan == command_chan).as_option().context(UnknownChannel{ chan: String::from(chan) })?;
        for msg in msgs {
            if msg.starts_with("rate ") {
                match state.last_status() {
                    PlayerStatus::Stopped => {
                        warn!("Player is stopped-- can't rate current track.");
                    }
                    PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                        for msg in msgs {
                            match handle_rating(
                                &msg[5..],
                                client,
                                &rating_sticker,
                                &ratings_cmd,
                                &ratings_cmd_args,
                                &music_dir,
                                &curr.file,
                            )
                                .await
                            {
                                Ok(opt) => match opt {
                                    Some(fut) => cmds.push(fut),
                                    None => {}
                                },
                                Err(err) => warn!("{:#?}", err),
                            }
                        }
                    }
                } // Close `match'
            } else if msg.starts_with("send ") {
                match state.last_status() {
                    PlayerStatus::Stopped => {
                        warn!("Player is stopped-- can't rate current track.");
                    }
                    PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                        let file = curr.file;
                        for msg in msgs {
                            client.send_to_playlist(file.to_str().context(BadPath{ pth: file.clone() })?, &msg).await?;
                        }
                    }
                }
            } else {
                return Err(Error::UnknownCommand{ msg: String::from(msg),
                                                  back: Backtrace::generate() });
            }
        }
    } // End `for'.

    Ok(())
}

// TODO(sp1ff): ugh
pub async fn mpdpopm(cfg: Config) -> std::result::Result<(), Error> {
    info!("mpdpopm {} beginning.", vars::VERSION);

    // TODO(sp1ff): Ugh
    let host = cfg.host;
    let port = cfg.port;
    let music_dir = cfg.local_music_dir;
    let rating_sticker = cfg.rating_sticker;
    let ratings_cmd = cfg.ratings_command;
    let ratings_cmd_args = cfg.ratings_command_args;
    let poll_interval_ms = cfg.poll_interval_ms;
    let playcount_sticker = cfg.playcount_sticker;
    let lastplayed_sticker = cfg.lastplayed_sticker;
    let playcount_cmd = cfg.playcount_command;
    let playcount_cmd_args = cfg.playcount_command_args;

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();

    let mut client = Client::connect(format!("{}:{}", host, port)).await?;
    let mut state = PlayState::new(&mut client, &playcount_sticker, &lastplayed_sticker,
                                   cfg.played_thresh).await?;

    let mut idle_client = IdleClient::connect(format!("{}:{}", host, port)).await?;
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
                    // TODO(sp1ff): figure out why Ctrl-C handling breaks at some point
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
                        state.update(&mut client, &playcount_cmd, &playcount_cmd_args, &music_dir,
                                     &mut cmds).await?;
                    },
                    res = idle => {
                        match res {
                            Ok(subsys) => {
                                debug!("subsystem {} changed", subsys);
                                if subsys == IdleSubSystem::Player {
                                    debug!("Updating status.");
                                    state.update(&mut client, &playcount_cmd, &playcount_cmd_args,
                                                 &music_dir, &mut cmds).await?;
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

        if msg_check_needed {
            check_messages(&mut client, &mut idle_client, &mut state, &cfg.commands_chan,
                           &rating_sticker, &ratings_cmd, &ratings_cmd_args,
                           &music_dir, &mut cmds).await?;
        }
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}
