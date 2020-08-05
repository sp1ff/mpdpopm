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

//! # mppopmd
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

use mpdpopm::error_from;
use mpdpopm::mpdpopm;
use mpdpopm::Config;

use clap::{App, Arg};
use log::{info, LevelFilter};
use log4rs::{
    append::{
        console::{ConsoleAppender, Target},
        file::FileAppender,
    },
    config::{Appender, Root},
    encode::pattern::PatternEncoder,
};
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};

use std::{fmt, path::PathBuf};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                 mppopmd application Error type                                 //
////////////////////////////////////////////////////////////////////////////////////////////////////

// Anything that implements std::process::Termination can be used as the return type for `main'. The
// std lib implements impl<E: Debug> Termination for Result<()undefined E> per
// <https://www.joshmcguigan.com/blog/custom-exit-status-codes-rust/>, so we're good-- just don't
// derive Debug-- implement by hand to produce something human-friendly (since this will be used for
// main's return value)
#[derive(Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display(
        "The config argument couldn't be retrieved. This is likely a bug; please \
consider filing a report with sp1ff@pobox.com"
    ))]
    NoConfigArg,
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
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

error_from!(mpdpopm::Error);
error_from!(serde_lexpr::error::Error);
error_from!(std::io::Error);

type Result = std::result::Result<(), Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         The Big Kahuna                                         //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[tokio::main]
async fn main() -> Result {
    use mpdpopm::vars::{AUTHOR, SYSCONFDIR, VERSION};

    let matches = App::new("mppopmd")
        .version(VERSION)
        .author(AUTHOR)
        .about("mpd + POPM")
        .long_about(
            "
`mppopmd' is a companion daemon for `mpd' that maintains playcounts & ratings,
as well as implementing some handy functions. It maintains ratings & playcounts in the sticker
databae, but it allows you to keep that information in your tags, as well, by invoking external
commands to keep your tags up-to-date.",
        )
        .arg(
            Arg::with_name("no-daemon")
                .short('F')
                .about("do not daemonize; remain in foreground"),
        )
        .arg(
            Arg::with_name("config")
                .short('c')
                .takes_value(true)
                .value_name("FILE")
                .default_value(&format!("{}/mppopmd.conf", SYSCONFDIR))
                .about("path to configuration file"),
        )
        .arg(
            Arg::with_name("verbose")
                .short('v')
                .about("enable verbose logging"),
        )
        .arg(
            Arg::with_name("debug")
                .short('d')
                .about("enable debug logging (implies --verbose)"),
        )
        .get_matches();

    // Handling the configuration file is a little touchy; if the user simply accepted the default,
    // and it's not there, that's fine: we just proceed with a defualt configuration. But if they
    // explicitly named a configuration file, and it's not there, they presumably want to know
    // about that.
    let cfgpth = matches.value_of("config").context(NoConfigArg {})?;
    let cfg = match std::fs::read_to_string(cfgpth) {
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

    let daemonize = !matches.is_present("no-daemon");

    let lf = match (matches.is_present("verbose"), matches.is_present("debug")) {
        (_, true) => LevelFilter::Trace,
        (true, false) => LevelFilter::Debug,
        _ => LevelFilter::Info,
    };

    let _log_handle = if daemonize {
        let app = FileAppender::builder()
            .encoder(Box::new(PatternEncoder::new("{d}|{M}|{f}|{l}|{m}{n}")))
            .build(&cfg.log)
            .unwrap();
        let lcfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("logfile", Box::new(app)))
            .build(Root::builder().appender("logfile").build(lf))
            .unwrap();
        log4rs::init_config(lcfg)
    } else {
        let app = ConsoleAppender::builder()
            .target(Target::Stdout)
            .encoder(Box::new(PatternEncoder::new("[{d}][{M}] {m}{n}")))
            .build();
        let lcfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(app)))
            .build(Root::builder().appender("stdout").build(lf))
            .unwrap();
        log4rs::init_config(lcfg)
    };

    info!("logging configured.");

    // TODO(sp1ff): daemonize here, if `daemonize' is true

    Ok(mpdpopm(cfg).await?)
}
