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
    error_from,
    playcounts::{get_last_played, get_play_count},
    ratings::get_rating,
};

use clap::{App, Arg};
use log::{debug, info, trace, LevelFilter};
use log4rs::{
    append::console::{ConsoleAppender, Target},
    config::{Appender, Root},
    encode::pattern::PatternEncoder,
};
use serde::{Deserialize, Serialize};
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};

use std::{fmt, path::PathBuf};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                 mppopm application Error Type                                  //
////////////////////////////////////////////////////////////////////////////////////////////////////

// NB we take care NOT to derive Debug here. This is because main returns a Result<(), Error>; in
// the case of an error, the stdlib will format the resulting error message using the Debug trait
// which, when derived, is rather ugly. We'll implement it by hand below to produce something more
// pleasant for human beings to read.
/// `mppopm` errors
#[derive(Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("No sub-command specified; try `mppopm --help'"))]
    NoSubCommand,
    #[snafu(display(
        "The `config' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoConfigArg,
    #[snafu(display(
        "The `rating' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoRating,
    #[snafu(display(
        "The `playcount' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoPlayCount,
    #[snafu(display(
        "The `last-played' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoLastPlayed,
    #[snafu(display(
        "While trying to read the configuration file `{:?}', got `{}'",
        config,
        cause
    ))]
    NoConfig {
        config: std::path::PathBuf,
        #[snafu(source(true))]
        cause: std::io::Error,
    },
    #[snafu(display("Can't retrieve the current song when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("Received a path with non-UTF8 codepoints: {:?}", path))]
    BadPath {
        path: PathBuf,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[cfg(feature = "scribbu")]
    #[snafu(display(
        "The `xtag' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoXtag,
    #[cfg(feature = "scribbu")]
    #[snafu(display(
        "The `genre' argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoGenre,
    #[snafu(display(
        "The `playlist' argument couldn't be retrieved. This is likely a bug; \
please consider filing a report with sp1ff@pobox.com"
    ))]
    NoPlaylist,
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

error_from!(log::SetLoggerError);
error_from!(mpdpopm::Error);
error_from!(mpdpopm::clients::Error);
error_from!(mpdpopm::playcounts::Error);
error_from!(mpdpopm::ratings::Error);
error_from!(serde_lexpr::error::Error);
error_from!(std::env::VarError);
error_from!(std::num::ParseIntError);

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
    /// [0,255]
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
/// This is a convenience function for mapping the value returned by [`values_of`] to a
/// convenient representation of the user's intentions.
///
/// [`values_of`]: [`clap::ArgMatches::values_of`]
async fn map_tracks<'a, Iter: Iterator<Item = &'a str>>(
    client: &mut Client,
    args: Option<Iter>,
) -> Result<Vec<String>> {
    let files = match args {
        Some(iter) => iter.map(|x| x.to_string()).collect(),
        None => {
            let file = match client.status().await? {
                PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr
                    .file
                    .to_str()
                    .context(BadPath {
                        path: curr.file.clone(),
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
async fn get_ratings<'a, Iter: Iterator<Item = &'a str>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut ratings: Vec<(String, u8)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let rating = get_rating(client, sticker, &file).await?;
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
    client.send_message(chan, &cmd).await?;

    match arg {
        Some(uri) => info!("Set the rating for \"{}\" to \"{}\".", uri, rating),
        None => info!("Set the rating for the current song to \"{}\".", rating),
    }

    Ok(())
}

/// Retrieve the playcount for one or more tracks
async fn get_play_counts<'a, Iter: Iterator<Item = &'a str>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut playcounts: Vec<(String, usize)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let playcount = match get_play_count(client, sticker, &file).await? {
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
    client.send_message(chan, &cmd).await?;

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
async fn get_last_playeds<'a, Iter: Iterator<Item = &'a str>>(
    client: &mut Client,
    sticker: &str,
    tracks: Option<Iter>,
    with_uri: bool,
) -> Result<()> {
    let mut lastplayeds: Vec<(String, Option<u64>)> = Vec::new();
    for file in map_tracks(client, tracks).await? {
        let lastplayed = get_last_played(client, sticker, &file).await?;
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
    client.send_message(chan, &cmd).await?;

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
    let mut pls = client.get_stored_playlists().await?;
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
    client.send_message(chan, &cmd).await?;
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
        .await?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////////////////////////

fn add_general_subcommands(app: App) -> App {
    app.subcommand(
        App::new("get-rating")
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
                Arg::with_name("with-uri")
                    .short('u')
                    .long("with-uri")
                    .about("Always show the song URI, even when there is only one track"),
            )
            .arg(Arg::with_name("track").multiple(true)),
    )
    .subcommand(
        App::new("set-rating")
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
            .arg(Arg::with_name("rating").index(1).required(true))
            .arg(Arg::with_name("track").index(2)),
    )
    .subcommand(
        App::new("get-pc")
            .about("retrieve the play count for one or more tracks")
            .long_about(
                "
With no arguments, retrieve the play count of the current song & print it
on stdout. With one argument, retrieve that track's play count & print it
on stdout. With multiple arguments, print their play counts on stdout, one
per line, prefixed by the track name.",
            )
            .arg(
                Arg::with_name("with-uri")
                    .short('u')
                    .long("with-uri")
                    .about("Always show the song URI, even when there is only one"),
            )
            .arg(Arg::with_name("track").multiple(true)),
    )
    .subcommand(
        App::new("set-pc")
            .about("set the play count for one track")
            .long_about(
                "
With one argument, set the play count of the current song to that argument. With a
second argument, set the play count for that song to the first.",
            )
            .arg(Arg::with_name("play-count").index(1).required(true))
            .arg(Arg::with_name("track").index(2)),
    )
    .subcommand(
        App::new("get-lp")
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
                Arg::with_name("with-uri")
                    .short('u')
                    .long("with-uri")
                    .about("Always show the song URI, even when there is only one"),
            )
            .arg(Arg::with_name("track").multiple(true)),
    )
    .subcommand(
        App::new("set-lp")
            .about("set the last played timestamp for one track")
            .long_about(
                "
With one argument, set the last played time of the current song. With two
arguments, set the last played time for the second argument to the first.
The last played timestamp is expressed in seconds since Unix epoch.",
            )
            .arg(Arg::with_name("last-played").index(1).required(true))
            .arg(Arg::with_name("track").index(2)),
    )
    .subcommand(App::new("get-playlists").about("retreive the list of stored playlists"))
    .subcommand(
        App::new("findadd")
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
            .arg(Arg::with_name("filter").index(1).required(true)),
    )
    .subcommand(
        App::new("searchadd")
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
            .arg(Arg::with_name("filter").index(1).required(true)),
    )
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         The Big Kahuna                                         //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[tokio::main]
async fn main() -> Result<()> {
    use mpdpopm::vars::{AUTHOR, VERSION};

    let def_cfg = format!("{}/.mppopm", std::env::var("HOME")?);
    let mut app = App::new("mppopm")
        .version(VERSION)
        .author(AUTHOR)
        .about("`mppopmd' client")
        .arg(
            Arg::with_name("verbose")
                .short('v')
                .long("verbose")
                .about("enable verbose logging"),
        )
        .arg(
            Arg::with_name("debug")
                .short('d')
                .long("debug")
                .about("enable debug logging (implies --verbose)"),
        )
        .arg(
            Arg::with_name("config")
                .short('c')
                .takes_value(true)
                .value_name("FILE")
                .default_value(&def_cfg)
                .about("path to configuration file"),
        )
        .arg(
            Arg::with_name("host")
                .short('H')
                .takes_value(true)
                .value_name("HOST")
                .about("MPD host"),
        )
        .arg(
            Arg::with_name("port")
                .short('p')
                .takes_value(true)
                .value_name("PORT")
                .about("MPD port"),
        );

    app = add_general_subcommands(app);
    app = app.arg(Arg::with_name("args").multiple(true));

    let matches = app.get_matches();

    // Handling the configuration file is a little touchy; if the user simply accepted the default,
    // and it's not there, that's fine: we just proceed with a defualt configuration. But if they
    // explicitly named a configuration file, and it's not there, they presumably want to know
    // about that.
    let cfgpth = matches.value_of("config").context(NoConfigArg {})?;
    let mut cfg = match std::fs::read_to_string(cfgpth) {
        // The config file (defaulted or not) existed & we were able to read its contents-- parse
        // em!
        Ok(text) => serde_lexpr::from_str(&text)?,
        // The config file (defaulted or not) either didn't exist, or we were unable to read its
        // contents...
        Err(err) => match (err.kind(), matches.occurrences_of("config")) {
            (std::io::ErrorKind::NotFound, 0) => {
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

    match matches.value_of("host") {
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

    match matches.value_of("port") {
        Some(port) => {
            cfg.port = port.parse::<u16>()?;
        }
        None => {
            if cfg.port == 0 {
                cfg.port = match std::env::var("MPD_PORT") {
                    Ok(port) => port.parse::<u16>()?,
                    Err(_) => 6600,
                }
            }
        }
    }

    // Handle log verbosity: debug => verbose
    let lf = match (matches.is_present("verbose"), matches.is_present("debug")) {
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
    log4rs::init_config(lcfg)?;

    trace!("logging configured.");

    // Whatever we do, we're going to need a Client, so whip one up now:
    let mut client = Client::connect(format!("{}:{}", cfg.host, cfg.port)).await?;

    if let Some(subm) = matches.subcommand_matches("get-rating") {
        return Ok(get_ratings(
            &mut client,
            &cfg.rating_sticker,
            subm.values_of("track"),
            subm.is_present("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-rating") {
        return Ok(set_rating(
            &mut client,
            &cfg.commands_chan,
            subm.value_of("rating").context(NoRating {})?,
            subm.value_of("track"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("get-pc") {
        return Ok(get_play_counts(
            &mut client,
            &cfg.playcount_sticker,
            subm.values_of("track"),
            subm.is_present("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-pc") {
        return Ok(set_play_counts(
            &mut client,
            &cfg.commands_chan,
            subm.value_of("play-count")
                .context(NoPlayCount {})?
                .parse::<usize>()?,
            subm.value_of("track"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("get-lp") {
        return Ok(get_last_playeds(
            &mut client,
            &cfg.lastplayed_sticker,
            subm.values_of("track"),
            subm.is_present("with-uri"),
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("set-lp") {
        return Ok(set_last_playeds(
            &mut client,
            &cfg.commands_chan,
            subm.value_of("last-played")
                .context(NoLastPlayed {})?
                .parse::<u64>()?,
            subm.value_of("track"),
        )
        .await?);
    } else if let Some(_subm) = matches.subcommand_matches("get-playlists") {
        return Ok(get_playlists(&mut client).await?);
    } else if let Some(subm) = matches.subcommand_matches("findadd") {
        return Ok(findadd(
            &mut client,
            &cfg.commands_chan,
            // filter is mandatory
            subm.value_of("filter").unwrap(),
            true,
        )
        .await?);
    } else if let Some(subm) = matches.subcommand_matches("searchadd") {
        return Ok(findadd(
            &mut client,
            &cfg.commands_chan,
            // filter is mandatory
            subm.value_of("filter").unwrap(),
            false,
        )
        .await?);
    } else if let Some(args) = matches.values_of("args") {
        return Ok(send_command(&mut client, &cfg.commands_chan, args).await?);
    }

    Err(Error::NoSubCommand)
}
