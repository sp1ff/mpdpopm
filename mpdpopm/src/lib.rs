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
//! I'm currently sending all commands over one (configurable) channel.
//!

#![recursion_limit = "512"] // for the `select!' macro

pub mod clients;
pub mod commands;
pub mod error_from;
pub mod messages;
pub mod playcounts;
pub mod ratings;
pub mod vars;

use clients::{Client, IdleClient, IdleSubSystem};
use commands::{FormalParameter, GeneralizedCommand, TaggedCommandFuture, Update};
use messages::MessageProcessor;
use playcounts::PlayState;
use vars::{LOCALSTATEDIR, PREFIX};

use futures::{
    future::FutureExt,
    pin_mut, select,
    stream::{FuturesUnordered, StreamExt},
};
use log::{debug, error, info};
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
    #[snafu(display("The path `{}' cannot be converted to a UTF-8 string", pth.display()))]
    BadPath { pth: PathBuf },
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

error_from!(commands::Error);
error_from!(clients::Error);
error_from!(playcounts::Error);
error_from!(ratings::Error);
error_from!(std::num::ParseIntError);
error_from!(std::time::SystemTimeError);

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Serialize, Deserialize, Debug)]
pub struct GeneralizedCommandDefn {
    /// Command name
    name: String,
    /// An ordered collection of formal parameter types
    formal_parameters: Vec<FormalParameter>,
    /// Actual parameters may be defaulted after this many places
    default_after: usize,
    /// The command to be run; if not absolute, the `PATH` will be searched in an system-
    /// dependent way
    cmd: PathBuf,
    /// Command arguments; may include replacement parameters (like "%full-file")
    args: Vec<String>,
    /// The sort of MPD music database update that needs to take place when this command finishes
    update: Update,
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
    /// Generalized commands
    gen_cmds: Vec<GeneralizedCommandDefn>,
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
            gen_cmds: Vec::<GeneralizedCommandDefn>::new(),
        }
    }
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

    let mproc = MessageProcessor::new(
        &music_dir,
        &cfg.rating_sticker,
        &cfg.ratings_command,
        cfg.ratings_command_args.iter().cloned(),
        &cfg.playcount_sticker,
        &cfg.playcount_command,
        cfg.playcount_command_args.iter().cloned(),
        &cfg.lastplayed_sticker,
        cfg.gen_cmds.iter().map(|defn| {
            (
                defn.name.clone(),
                GeneralizedCommand::new(
                    defn.formal_parameters.iter().cloned(),
                    defn.default_after,
                    &music_dir,
                    &defn.cmd,
                    defn.args.iter().cloned(),
                    defn.update,
                ),
            )
        }),
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
        } // `idle_client' mutable borrow dropped here, which is important...

        // because it's mutably borrowed again here:
        if msg_check_needed {
            // Check for any messages that have come in; if there's an error there's not a lot we
            // can do about it (suppose some client fat-fingers a command name, e.g.)-- just log it
            // & move on.
            if let Err(err) = mproc
                .check_messages(
                    &mut client,
                    &mut idle_client,
                    state.last_status(),
                    &cfg.commands_chan,
                    &mut cmds,
                )
                .await
            {
                error!("{}", err);
            }
        }
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}
