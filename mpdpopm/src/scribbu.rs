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

//! scribbu -- aditional daemon commands that rely on [scribbu](https://github.com/sp1ff/scribbu).
//!
//! # Introduction
//!
//! [`mpdpopm`] accepts commands and generally implements them in terms of
//! [mpd](https://www.musicpd.org/).  As I developed [`mpdpopm`] & used it, I found it convenient to
//! add additional commands that could be implemented by running a little utility I wrote called
//! [scribbu](https://github.com/sp1ff/scribbu). Since most users won't have
//! [scribbu](https://github.com/sp1ff/scribbu) installed, I've moved support for these commands
//! here & placed them behind a feature flag.

use crate::clients::{Client, PlayerStatus};
use crate::commands::TaggedCommandFuture;

use log::debug;
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};
use tokio::process::Command;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                       module Error type                                        //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Cannot apply a command to the current track when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("Path contains non-UTF8 codepoints"))]
    BadPath {
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Mal-formed command `{}'", cmd))]
    BadCommand {
        cmd: String,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// "setgenre GENRE( TRACK)?"
#[cfg(feature = "scribbu")]
pub fn set_genre(
    msg: &str,
    _client: &mut Client,
    state: &PlayerStatus,
    music_dir: &str,
) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
    let args: Vec<_> = msg.split(char::is_whitespace).collect();
    if args.len() < 1 {
        return Err(Error::BadCommand {
            cmd: msg.to_string(),
            back: Backtrace::generate(),
        });
    }

    // We'll invoke "scribbu genre -a -C -g <genre> <file>"
    let mut a: Vec<String> = vec![
        "genre".to_string(),
        "-a".to_string(),
        "-C".to_string(),
        "-g".to_string(),
        args[0].to_string(),
    ];
    let song = if args.len() > 2 {
        args[2]
    } else {
        state
            .current_song()
            .context(PlayerStopped {})?
            .file
            .to_str()
            .context(BadPath {})?
    };
    a.push(format!("{}/{}", music_dir, song));
    if args.len() > 3 {
        return Err(Error::BadCommand {
            cmd: msg.to_string(),
            back: Backtrace::generate(),
        });
    }

    Ok(Some(TaggedCommandFuture::pin(
        Box::pin(Command::new("scribbu").args(a).output()),
        Some(song.to_string()),
    ))) // Update the DB when `set-genre' completes
}

/// "setxtag URL-ENCODED-TAG-CLOUD( MERGE( TRACK)?)?
#[cfg(feature = "scribbu")]
pub fn set_xtag(
    msg: &str,
    _client: &mut Client,
    state: &PlayerStatus,
    music_dir: &str,
) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
    let args: Vec<_> = msg.split(char::is_whitespace).collect();
    let mut a: Vec<String> = vec!["xtag".to_string(), "-A".to_string()];
    if args.len() > 1 {
        match args[1] {
            "no" | "0" | "false" => a.push(String::from("-a")),
            _ => a.push(String::from("-m")),
        }
    }
    a.push(String::from("-o"));
    a.push(String::from("sp1ff@pobox.com"));
    a.push(String::from("-T"));
    a.push(String::from(args[0]));
    let song = if args.len() > 2 {
        args[2]
    } else {
        state
            .current_song()
            .context(PlayerStopped {})?
            .file
            .to_str()
            .context(BadPath {})?
    };
    a.push(format!("{}/{}", music_dir, song));
    if args.len() > 3 {
        return Err(Error::BadCommand {
            cmd: msg.to_string(),
            back: Backtrace::generate(),
        });
    }

    debug!("scribbu xtags args: {:#?}", a);

    Ok(Some(TaggedCommandFuture::pin(
        Box::pin(Command::new("scribbu").args(a).output()),
        None,
    ))) // No need to update the DB
}
