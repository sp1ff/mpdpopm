// Copyright (C) 2021-2025 Michael herstine <sp1ff@pobox.com>
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

//! # mpdpopm Configuration
//!
//! ## Introduction
//!
//! This module defines the configuration struct & handles deserialization thereof.
//!
//! ## Discussion
//!
//! In the first releases of [mpdpopm](crate) I foolishly forgot to add a version field to the
//! configuration structure. I am now paying for my sin by having to attempt serializing two
//! versions until one succeeds.
//!
//! The idiomatic approach to versioning [serde](https://docs.serde.rs/serde/) structs seems to be
//! using an
//! [enumeration](https://www.reddit.com/r/rust/comments/44dds3/handling_multiple_file_versions_with_serde_or/). This
//! implementation *now* uses that, but that leaves us with the problem of handling the initial,
//! un-tagged version. I proceed as follows:
//!
//!    1. attempt to deserialize as a member of the modern enumeration
//!    2. if that succeeds, with the most-recent version, we're good
//!    3. if that succeeds with an archaic version, convert to the most recent and warn the user
//!    4. if that fails, attempt to deserialize as the initial struct version
//!    5. if that succeeds, convert to the most recent & warn the user
//!    6. if that fails, I'm kind of stuck because I don't know what the user was trying to express;
//!    bundle-up all the errors, report 'em & urge the user to use the most recent version
use crate::commands::{FormalParameter, Update};
use crate::vars::{LOCALSTATEDIR, PREFIX};

use serde::{Deserialize, Serialize};
use snafu::Snafu;

use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug)]
pub struct GeneralizedCommandDefn {
    /// Command name
    pub name: String,
    /// An ordered collection of formal parameter types
    pub formal_parameters: Vec<FormalParameter>,
    /// Actual parameters may be defaulted after this many places
    pub default_after: usize,
    /// The command to be run; if not absolute, the `PATH` will be searched in an system-
    /// dependent way
    pub cmd: PathBuf,
    /// Command arguments; may include replacement parameters (like "%full-file")
    pub args: Vec<String>,
    /// The sort of MPD music database update that needs to take place when this command finishes
    pub update: Update,
}

/// This is the initial version of the `mppopmd` configuration struct.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config0 {
    /// Location of log file
    log: PathBuf,
    /// Host on which `mpd` is listening
    host: String,
    /// TCP port on which `mpd` is listening
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
    /// `[0,255]`
    rating_sticker: String,
    /// Command, with replacement parameters, to be run when a song is rated
    ratings_command: String,
    /// Args, with replacement parameters, for the ratings command
    ratings_command_args: Vec<String>,
    /// Generalized commands
    gen_cmds: Vec<GeneralizedCommandDefn>,
}

/// [mpdpopm](crate) can communicate with MPD over either a local Unix socket, or over regular TCP
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum Connection {
    /// Local Unix socket-- payload is the path to the socket
    Local { path: PathBuf },
    /// TCP-- payload is the hostname & port number
    TCP { host: String, port: u16 },
}

#[cfg(test)]
mod test_connection {
    use super::Connection;

    #[test]
    fn test_serde() {
        use serde_lexpr::to_string;

        use std::path::PathBuf;

        let text = to_string(&Connection::Local {
            path: PathBuf::from("/var/run/mpd.sock"),
        })
        .unwrap();
        assert_eq!(
            text,
            String::from(r#"(Local (path . "/var/run/mpd.sock"))"#)
        );
        let text = to_string(&Connection::TCP {
            host: String::from("localhost"),
            port: 6600,
        })
        .unwrap();
        assert_eq!(
            text,
            String::from(r#"(TCP (host . "localhost") (port . 6600))"#)
        );
    }
}

impl std::default::Default for Connection {
    fn default() -> Self {
        Connection::TCP {
            host: String::from("localhost"),
            port: 6600 as u16,
        }
    }
}

/// This is the most recent `mppopmd` configuration struct.
#[derive(Deserialize, Debug, Serialize)]
#[serde(default)]
pub struct Config {
    /// Configuration format version-- must be "1"
    // Workaround to https://github.com/rotty/lexpr-rs/issues/77
    // When this gets fixed, I can remove this element from the struct & deserialize as
    // a Configurations element-- the on-disk format will be the same.
    #[serde(rename = "version")]
    _version: String,
    /// Location of log file
    pub log: PathBuf,
    /// How to connect to mpd
    pub conn: Connection,
    /// The `mpd' root music directory, relative to the host on which *this* daemon is running
    pub local_music_dir: PathBuf,
    /// Sticker name under which to store playcounts
    pub playcount_sticker: String,
    /// Sticker name under which to store the last played timestamp
    pub lastplayed_sticker: String,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    pub played_thresh: f64,
    /// The interval, in milliseconds, at which to poll `mpd' for the current state
    pub poll_interval_ms: u64,
    /// Channel to setup for assorted commands-- channel names must satisfy "[-a-zA-Z-9_.:]+"
    pub commands_chan: String,
    /// Command, with replacement parameters, to be run when a song's playcount is incremented
    pub playcount_command: String,
    /// Args, with replacement parameters, for the playcount command
    pub playcount_command_args: Vec<String>,
    /// Sticker under which to store song ratings, as a textual representation of a number in
    /// `[0,255]`
    pub rating_sticker: String,
    /// Command, with replacement parameters, to be run when a song is rated
    pub ratings_command: String,
    /// Args, with replacement parameters, for the ratings command
    pub ratings_command_args: Vec<String>,
    /// Generalized commands
    pub gen_cmds: Vec<GeneralizedCommandDefn>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            _version: String::from("1"),
            log: [LOCALSTATEDIR, "log", "mppopmd.log"].iter().collect(),
            conn: Connection::default(),
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

impl From<Config0> for Config {
    fn from(cfg0: Config0) -> Self {
        Config {
            _version: String::from("1"),
            log: cfg0.log,
            conn: Connection::TCP {
                host: cfg0.host,
                port: cfg0.port,
            },
            local_music_dir: cfg0.local_music_dir,
            playcount_sticker: cfg0.playcount_sticker,
            lastplayed_sticker: cfg0.lastplayed_sticker,
            played_thresh: cfg0.played_thresh,
            poll_interval_ms: cfg0.poll_interval_ms,
            commands_chan: cfg0.commands_chan,
            playcount_command: cfg0.playcount_command,
            playcount_command_args: cfg0.playcount_command_args,
            rating_sticker: cfg0.rating_sticker,
            ratings_command: cfg0.ratings_command,
            ratings_command_args: cfg0.ratings_command_args,
            gen_cmds: cfg0.gen_cmds,
        }
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    /// Failure to parse
    #[snafu(display("Parse failure: {outer}"))]
    ParseFail {
        outer: Box<dyn std::error::Error>,
        inner: Box<dyn std::error::Error>,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn from_str(text: &str) -> Result<Config> {
    let cfg: Config = match serde_lexpr::from_str(&text) {
        Ok(cfg) => cfg,
        Err(err_outer) => {
            let fallback: serde_lexpr::Result<Config0> = serde_lexpr::from_str(&text);
            match fallback {
                Ok(cfg) => Config::from(cfg),
                Err(err_inner) => {
                    return Err(Error::ParseFail {
                        outer: Box::new(err_outer),
                        inner: Box::new(err_inner),
                    })
                }
            }
        }
    };
    Ok(cfg)
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_from_str() {
        let cfg = Config::default();
        assert_eq!(cfg.commands_chan, String::from("unwoundstack.com:commands"));

        assert_eq!(
            serde_lexpr::to_string(&cfg).unwrap(),
            format!(
                r#"((version . "1") (log . "{}/log/mppopmd.log") (conn TCP (host . "localhost") (port . 6600)) (local_music_dir . "{}/Music") (playcount_sticker . "unwoundstack.com:playcount") (lastplayed_sticker . "unwoundstack.com:lastplayed") (played_thresh . 0.6) (poll_interval_ms . 5000) (commands_chan . "unwoundstack.com:commands") (playcount_command . "") (playcount_command_args) (rating_sticker . "unwoundstack.com:rating") (ratings_command . "") (ratings_command_args) (gen_cmds))"#,
                LOCALSTATEDIR, PREFIX
            )
        );

        let cfg: Config = serde_lexpr::from_str(
            r#"
((version . "1")
 (log . "/usr/local/var/log/mppopmd.log")
 (conn TCP (host . "localhost") (port . 6600))
 (local_music_dir . "/usr/local/Music")
 (playcount_sticker . "unwoundstack.com:playcount")
 (lastplayed_sticker . "unwoundstack.com:lastplayed")
 (played_thresh . 0.6)
 (poll_interval_ms . 5000)
 (commands_chan . "unwoundstack.com:commands")
 (playcount_command . "")
 (playcount_command_args)
 (rating_sticker . "unwoundstack.com:rating")
 (ratings_command . "")
 (ratings_command_args)
 (gen_cmds))
"#,
        )
        .unwrap();
        assert_eq!(cfg._version, String::from("1"));

        let cfg: Config = serde_lexpr::from_str(
            r#"
((version . "1")
 (log . "/usr/local/var/log/mppopmd.log")
 (conn Local (path . "/home/mgh/var/run/mpd/mpd.sock"))
 (local_music_dir . "/usr/local/Music")
 (playcount_sticker . "unwoundstack.com:playcount")
 (lastplayed_sticker . "unwoundstack.com:lastplayed")
 (played_thresh . 0.6)
 (poll_interval_ms . 5000)
 (commands_chan . "unwoundstack.com:commands")
 (playcount_command . "")
 (playcount_command_args)
 (rating_sticker . "unwoundstack.com:rating")
 (ratings_command . "")
 (ratings_command_args)
 (gen_cmds))
"#,
        )
        .unwrap();
        assert_eq!(cfg._version, String::from("1"));
        assert_eq!(
            cfg.conn,
            Connection::Local {
                path: PathBuf::from("/home/mgh/var/run/mpd/mpd.sock")
            }
        );

        // Test fallback to "v0" of the config struct
        let cfg = from_str(r#"
((log . "/home/mgh/var/log/mppopmd.log")
 (host . "192.168.1.14")
 (port . 6600)
 (local_music_dir . "/space/mp3")
 (playcount_sticker . "unwoundstack.com:playcount")
 (lastplayed_sticker . "unwoundstack.com:lastplayed")
 (played_thresh . 0.6)
 (poll_interval_ms . 5000)
 (playcount_command . "/usr/local/bin/scribbu")
 (playcount_command_args . ("popm" "-v" "-a" "-f" "-o" "sp1ff@pobox.com" "-C" "%playcount" "%full-file"))
 (commands_chan . "unwoundstack.com:commands")
 (rating_sticker . "unwoundstack.com:rating")
 (ratings_command . "/usr/local/bin/scribbu")
 (ratings_command_args . ("popm" "-v" "-a" "-f" "-o" "sp1ff@pobox.com" "-r" "%rating" "%full-file"))
 (gen_cmds .
	   (((name . "set-genre")
	     (formal_parameters . (Literal Track))
	     (default_after . 1)
	     (cmd . "/usr/local/bin/scribbu")
	     (args . ("genre" "-a" "-C" "-g" "%1" "%full-file"))
	     (update . TrackOnly))
	    ((name . "set-xtag")
	     (formal_parameters . (Literal Track))
	     (default_after . 1)
	     (cmd . "/usr/local/bin/scribbu")
	     (args . ("xtag" "-A" "-o" "sp1ff@pobox.com" "-T" "%1" "%full-file"))
	     (update . TrackOnly))
	    ((name . "merge-xtag")
	     (formal_parameters . (Literal Track))
	     (default_after . 1)
	     (cmd . "/usr/local/bin/scribbu")
	     (args . ("xtag" "-m" "-o" "sp1ff@pobox.com" "-T" "%1" "%full-file"))
	     (update . TrackOnly)))))
"#).unwrap();
        assert_eq!(cfg.log, PathBuf::from("/home/mgh/var/log/mppopmd.log"));
    }
}
