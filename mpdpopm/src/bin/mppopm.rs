// Copyright (C) 2020-2023 Michael Herstine <sp1ff@pobox.com>
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

//! # mppopm
//!
//! mppopmd client
//!
//! # Introduction
//!
//! `mppopmd` is a companion daemon for [mpd](https://www.musicpd.org/) that maintains play counts &
//! ratings. Similar to [mpdfav](https://github.com/vincent-petithory/mpdfav), but written in Rust
//! (which I prefer to Go), it will allow you to maintain that information in your tags, as well as
//! the sticker database, by invoking external commands to keep your tags up-to-date (something
//! along the lines of [mpdcron](https://alip.github.io/mpdcron)). `mppopm` is a command-line client
//! for `mppopmd`. Run `mppopm --help` for detailed usage.

use mpdpopm::{
    clients::{quote, Client, PlayerStatus},
    playcounts::{get_last_played, get_play_count},
    ratings::get_rating,
};

use backtrace::Backtrace;
use clap::{value_parser, Arg, ArgAction, Command};
use lazy_static::lazy_static;
use log::{debug, info, trace, LevelFilter};
use log4rs::{
    append::console::{ConsoleAppender, Target},
    config::{Appender, Root},
    encode::pattern::PatternEncoder,
};
use serde::{Deserialize, Serialize};

use std::{fmt, path::PathBuf};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                 mppopm application Error Type                                  //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[non_exhaustive]
pub enum Error {
    NoSubCommand,
    NoConfigArg,
    NoRating,
    NoPlayCount,
    NoLastPlayed,
    NoConfig {
        config: std::path::PathBuf,
        cause: std::io::Error,
    },
    PlayerStopped,
    BadPath {
        path: PathBuf,
        back: Backtrace,
    },
    NoPlaylist,
    Client {
        source: mpdpopm::clients::Error,
        back: Backtrace,
    },
    Ratings {
        source: mpdpopm::ratings::Error,
        back: Backtrace,
    },
    Playcounts {
        source: mpdpopm::playcounts::Error,
        back: Backtrace,
    },
    ExpectedInt {
        source: std::num::ParseIntError,
        back: Backtrace,
    },
    Logging {
        source: log::SetLoggerError,
        back: Backtrace,
    },
    Config {
        source: serde_lexpr::Error,
        back: Backtrace,
    },
}

impl fmt::Display for Error {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::NoSubCommand => write!(f, "No sub-command given"),
            Error::NoConfigArg => write!(f, "No argument given for the configuration option"),
            Error::NoRating => write!(f, "No rating supplied"),
            Error::NoPlayCount => write!(f, "No play count supplied"),
            Error::NoLastPlayed => write!(f, "No last played timestamp given"),
            Error::NoConfig { config, cause } => write!(f, "Bad config ({:?}): {}", config, cause),
            Error::PlayerStopped => write!(f, "The player is stopped"),
            Error::BadPath { path, back: _ } => write!(f, "Bad path: {:?}", path),
            Error::NoPlaylist => write!(f, "No playlist given"),
            Error::Client { source, back: _ } => write!(f, "Client error: {}", source),
            Error::Ratings { source, back: _ } => write!(f, "Rating error: {}", source),
            Error::Playcounts { source, back: _ } => write!(f, "Playcount error: {}", source),
            Error::ExpectedInt { source, back: _ } => write!(f, "Expected integer: {}", source),
            Error::Logging { source, back: _ } => write!(f, "Logging error: {}", source),
            Error::Config { source, back: _ } => {
                write!(f, "Error reading configuration: {}", source)
            }
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// `mppopm` configuration.
///
/// I'm using a separate configuration file for the client to support system-wide daemon installs
/// (in `/usr/local`, say) along with per-user client configurations (`~/.mppopm`, e.g.).
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    /// Host on which `mpd' is listening
    host: String,
    /// TCP port on which `mpd' is listening
    port: u16,
    /// Sticker name under which to store playcounts
    playcount_sticker: String,
    /// Sticker name under which to store the last played timestamp
    lastplayed_sticker: String,
    /// Channel to setup for assorted commands-- channel names must satisfy "[-a-zA-Z-9_.:]+"
    commands_chan: String,
    /// Sticker under which to store song ratings, as a textual representation of a number in
    /// `[0,255]`
    rating_sticker: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            host: String::new(),
            port: 0,
            playcount_sticker: String::from("unwoundstack.com:playcount"),
            lastplayed_sticker: String::from("unwoundstack.com:lastplayed"),
            commands_chan: String::from("unwoundstack.com:commands"),
            rating_sticker: String::from("unwoundstack.com:rating"),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           utilities                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Map `tracks' argument(s) to a Vec of String containing one or more mpd URIs
///
/// Several sub-commands take zero or more positional arguments meant to name tracks, with the
/// convention that zero indicates that the sub-command should use the currently playing track.
/// This is a convenience function for mapping the value returned by [`get_many`] to a
/// convenient representation of the user's intentions.
///
/// [`get_many`]: [`clap::ArgMatches::get_many`]
async fn map_tracks<'a, Iter: Iterator<Item = &'a String>>(
    client: &mut Client,
    args: Option<Iter>,
) -> Result<Vec<String>> {
    let files = match args {
        Some(iter) => iter.map(|x| x.clone()).collect(),
        None => {
            let file = match client.status().await.map_err(|err| Error::Client {
                source: err,
                back: Backtrace::new(),
            })? {
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .ok_or_else(|| Error::BadPath {
                        path: curr.file.clone(),
                        back: Backtrace::new(),
                    })?
                    .to_string(),
                PlayerStatus::Stopped => {
                    return Err(Error::PlayerStopped);
                }
            };
            vec![file]
        }
    };
    Ok(files)
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                          sub-commands                                          //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Retrieve ratings for one or more tracks
async fn get_ratings<'a, Iter: Iterator<Item = &'a String>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut ratings: Vec<(String, u8)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let rating = get_rating(client, sticker, &file)
            .await
            .map_err(|err| Error::Ratings {
                source: err,
                back: Backtrace::new(),
            })?;
        ratings.push((file, rating));
    }

    if ratings.len() == 1 && !with_uri {
        println!("{}", ratings[0].1);
    } else {
        for pair in ratings {
            println!("{}: {}", pair.0, pair.1);
        }
    }

    Ok(())
}

/// Rate a track
async fn set_rating(
    client: &mut Client,
    chan: &str,
    rating: &str,
    arg: Option<&str>,
) -> Result<()> {
    let cmd = match arg {
        Some(uri) => format!("rate {} \\\"{}\\\"", rating, uri),
        None => format!("rate {}", rating),
    };
    client
        .send_message(chan, &cmd)
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;

    match arg {
        Some(uri) => info!("Set the rating for \"{}\" to \"{}\".", uri, rating),
        None => info!("Set the rating for the current song to \"{}\".", rating),
    }

    Ok(())
}

/// Retrieve the playcount for one or more tracks
async fn get_play_counts<'a, Iter: Iterator<Item = &'a String>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut playcounts: Vec<(String, usize)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let playcount = match get_play_count(client, sticker, &file)
            .await
            .map_err(|err| Error::Playcounts {
                source: err,
                back: Backtrace::new(),
            })? {
            Some(pc) => pc,
            None => 0,
        };
        playcounts.push((file, playcount));
    }

    if playcounts.len() == 1 && !with_uri {
        println!("{}", playcounts[0].1);
    } else {
        for pair in playcounts {
            println!("{}: {}", pair.0, pair.1);
        }
    }

    Ok(())
}

/// Set the playcount for a track
async fn set_play_counts(
    client: &mut Client,
    chan: &str,
    playcount: usize,
    arg: Option<&str>,
) -> Result<()> {
    let cmd = match arg {
        Some(uri) => format!("setpc {} \\\"{}\\\"", playcount, uri),
        None => format!("setpc {}", playcount),
    };
    client
        .send_message(chan, &cmd)
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;

    match arg {
        Some(uri) => info!("Set the playcount for \"{}\" to \"{}\".", uri, playcount),
        None => info!(
            "Set the playcount for the current song to \"{}\".",
            playcount
        ),
    }

    Ok(())
}

/// Retrieve the last played time for one or more tracks
async fn get_last_playeds<'a, Iter: Iterator<Item = &'a String>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut lastplayeds: Vec<(String, Option<u64>)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let lastplayed = get_last_played(client, sticker, &file)
            .await
            .map_err(|err| Error::Playcounts {
                source: err,
                back: Backtrace::new(),
            })?;
        lastplayeds.push((file, lastplayed));
    }

    if lastplayeds.len() == 1 && !with_uri {
        println!(
            "{}",
            match lastplayeds[0].1 {
                Some(t) => format!("{}", t),
                None => String::from("N/A"),
            }
        );
    } else {
        for pair in lastplayeds {
            println!(
                "{}: {}",
                pair.0,
                match pair.1 {
                    Some(t) => format!("{}", t),
                    None => String::from("N/A"),
                }
            );
        }
    }

    Ok(())
}

/// Set the playcount for a track
async fn set_last_playeds(
    client: &mut Client,
    chan: &str,
    lastplayed: u64,
    arg: Option<&str>,
) -> Result<()> {
    let cmd = match arg {
        Some(uri) => format!("setlp {} {}", lastplayed, uri),
        None => format!("setlp {}", lastplayed),
    };
    client
        .send_message(chan, &cmd)
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;

    match arg {
        Some(uri) => info!("Set last played for \"{}\" to \"{}\".", uri, lastplayed),
        None => info!(
            "Set last played for the current song to \"{}\".",
            lastplayed
        ),
    }

    Ok(())
}

/// Retrieve the list of stored playlists
async fn get_playlists(client: &mut Client) -> Result<()> {
    let mut pls = client
        .get_stored_playlists()
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;
    pls.sort();
    println!("Stored playlists:");
    for pl in pls {
        println!("{}", pl);
    }
    Ok(())
}

/// Add songs selected by filter to the queue
async fn findadd(client: &mut Client, chan: &str, filter: &str, case: bool) -> Result<()> {
    let qfilter = quote(filter);
    debug!("findadd: got ``{}'', quoted to ``{}''.", filter, qfilter);
    let cmd = format!("{} {}", if case { "findadd" } else { "searchadd" }, qfilter);
    client
        .send_message(chan, &cmd)
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;
    Ok(())
}

/// Send an arbitrary command
async fn send_command<'a, A>(client: &mut Client, chan: &str, args: A) -> Result<()>
where
    A: Iterator<Item = &'a str>,
{
    client
        .send_message(
            chan,
            &format!(
                "{}",
                args.map(|a| quote(a)).collect::<Vec<String>>().join(" ")
            ),
        )
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////////////////////////

fn add_general_subcommands(app: Command) -> Command {
    app.subcommand(
        Command::new("get-rating")
            .about("retrieve the rating for one or more tracks")
            .long_about(
                "
With no arguments, retrieve the rating of the current song & print it
on stdout. With one argument, retrieve that track's rating & print it
on stdout. With multiple arguments, print their ratings on stdout, one
per line, prefixed by the track name.

Ratings are expressed as an integer between 0 & 255, inclusive, with
the convention that 0 denotes \"un-rated\".",
            )
            .arg(
                Arg::new("with-uri")
                    .short('u')
                    .long("with-uri")
                    .help("Always show the song URI, even when there is only one track")
                    .num_args(0)
                    .action(ArgAction::SetTrue),
            )
            .arg(Arg::new("track").num_args(0..)),
    )
    .subcommand(
        Command::new("set-rating")
            .about("set the rating for one track")
            .long_about(
                "
With one argument, set the rating of the current song to that argument. 
With a second argument, rate that song at the first argument. Ratings 
may be expressed as either an integer between 0 & 255, inclusive, 
or as one to five \"stars\" (asterisks). Stars are mapped to integers 
per the Winamp convention:

    *       1
    **     64
    ***   128
    ****  196
    ***** 255
",
            )
            .arg(Arg::new("rating").index(1).required(true))
            .arg(Arg::new("track").index(2)),
    )
    .subcommand(
        Command::new("get-pc")
            .about("retrieve the play count for one or more tracks")
            .long_about(
                "
With no arguments, retrieve the play count of the current song & print it
on stdout. With one argument, retrieve that track's play count & print it
on stdout. With multiple arguments, print their play counts on stdout, one
per line, prefixed by the track name.",
            )
            .arg(
                Arg::new("with-uri")
                    .short('u')
                    .long("with-uri")
                    .help("Always show the song URI, even when there is only one")
                    .num_args(0)
                    .action(ArgAction::SetTrue),
            )
            .arg(Arg::new("track").num_args(0..)),
    )
    .subcommand(
        Command::new("set-pc")
            .about("set the play count for one track")
            .long_about(
                "
With one argument, set the play count of the current song to that argument. With a
second argument, set the play count for that song to the first.",
            )
            .arg(Arg::new("play-count").index(1).required(true))
            .arg(Arg::new("track").index(2)),
    )
    .subcommand(
        Command::new("get-lp")
            .about("retrieve the last played timestamp for one or more tracks")
            .long_about(
                "
With no arguments, retrieve the last played timestamp of the current
song & print it on stdout. With one argument, retrieve that track's
last played time & print it on stdout. With multiple arguments, print
their last played times on stdout, one per line, prefixed by the track
name.

The last played timestamp is expressed in seconds since Unix epoch.",
            )
            .arg(
                Arg::new("with-uri")
                    .short('u')
                    .long("with-uri")
                    .help("Always show the song URI, even when there is only one")
                    .num_args(0)
                    .action(ArgAction::SetTrue),
            )
            .arg(Arg::new("track").num_args(0..)),
    )
    .subcommand(
        Command::new("set-lp")
            .about("set the last played timestamp for one track")
            .long_about(
                "
With one argument, set the last played time of the current song. With two
arguments, set the last played time for the second argument to the first.
The last played timestamp is expressed in seconds since Unix epoch.",
            )
            .arg(Arg::new("last-played").index(1).required(true))
            .arg(Arg::new("track").index(2)),
    )
    .subcommand(Command::new("get-playlists").about("retreive the list of stored playlists"))
    .subcommand(
        Command::new("findadd")
            .about("search case-sensitively for songs matching matching a filter and add them to the queue")
            .long_about(
                "
This command extends the MPD command `findadd' (which will search the MPD database) to allow
searches on attributes managed by mpdpopm: rating, playcount & last played time.

The MPD `findadd' <https://www.musicpd.org/doc/html/protocol.html#command-findadd> will search the
MPD database for songs that match a given filter & add them to the play queue. The filter syntax is
documented here <https://www.musicpd.org/doc/html/protocol.html#filter-syntax>.

This command adds three new terms on which you can filter: rating, playcount & lastplayed. Each is
expressed as an unsigned integer, with zero interpreted as \"not set\". For instance:

    mppopm findadd \"(rating > 128)\"

Will add all songs in the library with a rating sticker > 128 to the play queue.

mppopm also introduces OR clauses (MPD only supports AND), so that:

    mppopm findadd \"((rating > 128) AND (artist =~ \\\"pogues\\\"))\"

will add all songs whose artist tag matches the regexp \"pogues\" with a rating greater than
128.

`findadd' is case-sensitive; for case-insensitive searching see the `searchadd' command.
",
            )
            .arg(Arg::new("filter").index(1).required(true)),
    )
    .subcommand(
        Command::new("searchadd")
            .about("search case-insensitively for songs matching matching a filter and add them to the queue")
            .long_about(
                "
This command extends the MPD command `searchadd' (which will search the MPD database) to allow
searches on attributes managed by mpdpopm: rating, playcount & last played time.

The MPD `searchadd' <https://www.musicpd.org/doc/html/protocol.html#command-searchadd> will search
the MPD database for songs that match a given filter & add them to the play queue. The filter syntax
is documented here <https://www.musicpd.org/doc/html/protocol.html#filter-syntax>.

This command adds three new terms on which you can filter: rating, playcount & lastplayed. Each is
expressed as an unsigned integer, with zero interpreted as \"not set\". For instance:

    mppopm searchadd \"(rating > 128)\"

Will add all songs in the library with a rating sticker > 128 to the play queue.

mppopm also introduces OR clauses (MPD only supports AND), so that:

    mppopm searchadd \"((rating > 128) AND (artist =~ \\\"pogues\\\"))\"

will add all songs whose artist tag matches the regexp \"pogues\" with a rating greater than
128.

`searchadd' is case-insensitive; for case-sensitive searching see the `findadd' command.
",
            )
            .arg(Arg::new("filter").index(1).required(true)),
    )
}

lazy_static! {
    static ref DEF_CFG: String = format!(
        "{}/.mppopm",
        std::env::var("HOME").unwrap_or("".to_string())
    );
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         The Big Kahuna                                         //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[tokio::main]
async fn main() -> Result<()> {
    use mpdpopm::vars::{AUTHOR, VERSION};

    let mut app = Command::new("mppopm")
        .version(VERSION)
        .author(AUTHOR)
        .about("`mppopmd' client")
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .num_args(0)
                .action(ArgAction::SetTrue)
                .help("enable verbose logging"),
        )
        .arg(
            Arg::new("debug")
                .short('d')
                .long("debug")
                .num_args(0)
                .action(ArgAction::SetTrue)
                .help("enable debug logging (implies --verbose)"),
        )
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_parser(value_parser!(PathBuf))
                .value_name("FILE")
                .default_value(DEF_CFG.as_str())
                .help("path to configuration file"),
        )
        .arg(
            Arg::new("host")
                .short('H')
                .long("host")
                .value_parser(value_parser!(String))
                .value_name("HOST")
                .help("MPD host"),
        )
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .value_parser(value_parser!(u16))
                .value_name("PORT")
                .help("MPD port"),
        );

    app = add_general_subcommands(app);
    app = app.arg(Arg::new("args").num_args(0..));

    let matches = app.get_matches();

    // Handling the configuration file is a little touchy; if the user simply accepted the default,
    // and it's not there, that's fine: we just proceed with a defualt configuration. But if they
    // explicitly named a configuration file, and it's not there, they presumably want to know
    // about that.
    let cfgpth = matches
        .get_one::<PathBuf>("config")
        .ok_or_else(|| Error::NoConfigArg {})?;
    let mut cfg = match std::fs::read_to_string(cfgpth) {
        // The config file (defaulted or not) existed & we were able to read its contents-- parse
        // em!
        Ok(text) => serde_lexpr::from_str(&text).map_err(|err| Error::Config {
            source: err,
            back: Backtrace::new(),
        })?,
        // The config file (defaulted or not) either didn't exist, or we were unable to read its
        // contents...
        Err(err) => match (err.kind(), matches.value_source("config").unwrap()) {
            (std::io::ErrorKind::NotFound, clap::parser::ValueSource::DefaultValue) => {
                // The user just accepted the default option value & that default didn't exist; we
                // proceed with default configuration settings.
                Config::default()
            }
            (_, _) => {
                // Either they did _not_, in which case they probably want to know that the config
                // file they explicitly asked for does not exist, or there was some other problem,
                // in which case we're out of options, anyway. Either way:
                return Err(Error::NoConfig {
                    config: PathBuf::from(cfgpth),
                    cause: err,
                });
            }
        },
    };

    // The order of precedence for host & port is:

    //     1. command-line arguments
    //     2. configuration
    //     3. environment variable

    match matches.get_one::<String>("host") {
        Some(host) => {
            cfg.host = String::from(host);
        }
        None => {
            if cfg.host.is_empty() {
                cfg.host = match std::env::var("MPD_HOST") {
                    Ok(host) => String::from(host),
                    Err(_) => String::from("localhost"),
                }
            }
        }
    }

    match matches.get_one::<u16>("port") {
        Some(port) => cfg.port = *port,
        None => {
            if cfg.port == 0 {
                cfg.port = match std::env::var("MPD_PORT") {
                    Ok(port) => port.parse::<u16>().map_err(|err| Error::ExpectedInt {
                        source: err,
                        back: Backtrace::new(),
                    })?,
                    Err(_) => 6600,
                }
            }
        }
    }

    // Handle log verbosity: debug => verbose
    let lf = match (matches.get_flag("verbose"), matches.get_flag("debug")) {
        (_, true) => LevelFilter::Trace,
        (true, false) => LevelFilter::Debug,
        _ => LevelFilter::Warn,
    };

    let app = ConsoleAppender::builder()
        .target(Target::Stdout)
        .encoder(Box::new(PatternEncoder::new("{m}{n}")))
        .build();
    let lcfg = log4rs::config::Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(app)))
        .build(Root::builder().appender("stdout").build(lf))
        .unwrap();
    log4rs::init_config(lcfg).map_err(|err| Error::Logging {
        source: err,
        back: Backtrace::new(),
    })?;

    trace!("logging configured.");

    // Whatever we do, we're going to need a Client, so whip one up now:
    let mut client = Client::connect(format!("{}:{}", cfg.host, cfg.port))
        .await
        .map_err(|err| Error::Client {
            source: err,
            back: Backtrace::new(),
        })?;

    if let Some(subm) = matches.subcommand_matches("get-rating") {
        return Ok(get_ratings(
            &mut client,
            &cfg.rating_sticker,
            subm.get_many::<String>("track"),
            subm.get_flag("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-rating") {
        return Ok(set_rating(
            &mut client,
            &cfg.commands_chan,
            subm.get_one::<String>("rating")
                .ok_or_else(|| Error::NoRating {})?,
            subm.get_one::<String>("track")
                .as_deref()
                .map(|x| x.as_str()),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("get-pc") {
        return Ok(get_play_counts(
            &mut client,
            &cfg.playcount_sticker,
            subm.get_many::<String>("track"),
            subm.get_flag("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-pc") {
        return Ok(set_play_counts(
            &mut client,
            &cfg.commands_chan,
            subm.get_one::<String>("play-count")
                .ok_or_else(|| Error::NoPlayCount {})?
                .parse::<usize>()
                .map_err(|err| Error::ExpectedInt {
                    source: err,
                    back: Backtrace::new(),
                })?,
            subm.get_one::<String>("track")
                .as_deref()
                .map(|x| x.as_str()),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("get-lp") {
        return Ok(get_last_playeds(
            &mut client,
            &cfg.lastplayed_sticker,
            subm.get_many::<String>("track"),
            subm.get_flag("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-lp") {
        return Ok(set_last_playeds(
            &mut client,
            &cfg.commands_chan,
            subm.get_one::<String>("last-played")
                .ok_or_else(|| Error::NoLastPlayed {})?
                .parse::<u64>()
                .map_err(|err| Error::ExpectedInt {
                    source: err,
                    back: Backtrace::new(),
                })?,
            subm.get_one::<String>("track")
                .as_deref()
                .map(|x| x.as_str()),
        )
        .await?);
    } else if let Some(_subm) = matches.subcommand_matches("get-playlists") {
        return Ok(get_playlists(&mut client).await?);
    } else if let Some(subm) = matches.subcommand_matches("findadd") {
        return Ok(findadd(
            &mut client,
            &cfg.commands_chan,
            // filter is mandatory
            subm.get_one::<String>("filter").unwrap(),
            true,
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("searchadd") {
        return Ok(findadd(
            &mut client,
            &cfg.commands_chan,
            // filter is mandatory
            subm.get_one::<String>("filter").unwrap(),
            false,
        )
        .await?);
    } else if let Some(args) = matches.get_many::<String>("args") {
        return Ok(send_command(&mut client, &cfg.commands_chan, args.map(|x| x.as_str())).await?);
    }

    Err(Error::NoSubCommand)
}
