// Copyright (C) 2020-2021 Michael Herstine <sp1ff@pobox.com>
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
pub mod config;
pub mod error_from;
pub mod filters_ast;
pub mod messages;
pub mod playcounts;
pub mod ratings;
pub mod vars;

#[macro_use]
extern crate lalrpop_util;
lalrpop_mod!(pub filters); // synthesized by LALRPOP

use clients::{Client, IdleClient, IdleSubSystem};
use commands::{GeneralizedCommand, TaggedCommandFuture};
use config::Config;
use config::Connection;
use filters_ast::FilterStickerNames;
use messages::MessageProcessor;
use playcounts::PlayState;

use futures::{
    future::FutureExt,
    pin_mut, select,
    stream::{FuturesUnordered, StreamExt},
};
use libc::getpid;
use log::{debug, error, info};
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

/// Core `mppopmd' logic
pub async fn mpdpopm(cfg: Config) -> std::result::Result<(), Error> {
    let pid = unsafe { getpid() };
    info!("mpdpopm {} beginning (PID {}).", vars::VERSION, pid);

    // We need the music directory to be convertible to string; check that first-off:
    let music_dir = cfg.local_music_dir.to_str().context(BadPath {
        pth: cfg.local_music_dir.clone(),
    })?;

    let filter_stickers = FilterStickerNames::new(
        &cfg.rating_sticker,
        &cfg.playcount_sticker,
        &cfg.lastplayed_sticker,
    );

    let mut client = match cfg.conn {
        Connection::Local { ref path } => Client::open(path).await?,
        Connection::TCP { ref host, port } => Client::connect(format!("{}:{}", host, port)).await?,
    };

    let mut state = PlayState::new(
        &mut client,
        &cfg.playcount_sticker,
        &cfg.lastplayed_sticker,
        cfg.played_thresh,
    )
    .await?;

    let mut idle_client = match cfg.conn {
        Connection::Local { ref path } => IdleClient::open(path).await?,
        Connection::TCP { ref host, port } => {
            IdleClient::connect(format!("{}:{}", host, port)).await?
        }
    };

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
                    &filter_stickers,
                )
                .await
            {
                error!("Error while processing messages: {}", err);
            }
        }
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}
