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

use mpdpopm::config;
use mpdpopm::config::Config;
use mpdpopm::error_from;
use mpdpopm::mpdpopm;
use mpdpopm::vars::LOCALSTATEDIR;

use clap::{App, Arg};
use errno::errno;
use libc::{
    close, dup, exit, fork, getdtablesize, getpid, lockf, open, setsid, umask, write, F_TLOCK,
};
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

use std::{ffi::CString, fmt, path::PathBuf};

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
    #[snafu(display("Failed to fork this process: {}", errno))]
    Fork {
        errno: errno::Errno,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Failed to open /dev/null: {}", errno))]
    DevNull {
        errno: errno::Errno,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Path to lock file contained a null"))]
    PathContainsNull {
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Error opening lock file: {}", errno))]
    OpenLockFile {
        errno: errno::Errno,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Error locking lockfile: {}", errno))]
    LockFile {
        errno: errno::Errno,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("Error writing pid to lockfile: {}", errno))]
    WritePid {
        errno: errno::Errno,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

error_from!(log::SetLoggerError);
error_from!(mpdpopm::Error);
error_from!(mpdpopm::config::Error);
error_from!(serde_lexpr::error::Error);
error_from!(std::ffi::NulError);
error_from!(std::io::Error);

type Result = std::result::Result<(), Error>;

/// Make this process into a daemon
///
/// I first tried to use the `daemonize' crate, without success. Perhaps I wasn't using it
/// correctly. In any event, both to debug the issue(s) and for my own edification, I decided to
/// hand-code the daemonization process.
///
/// After spending a bit of time digging around the world of TTYs, process groups & sessions, I'm
/// beginning to understand what "daemon" means and how to create one. The first step is to
/// dissassociate this process from it's controlling terminal & make sure it cannot acquire a new
/// one. This, AFAIK, is to disconnect us from any job control associated with that terminal, and in
/// particular to prevent us from being disturbed when & if that terminal is closed (I'm still hazy
/// on the details, but at least the session leader (and perhaps it's descendants) will be sent a
/// SIGHUP in that eventuality).
///
/// After that, the rest of the work seems to consist of shedding all the things we (may have)
/// inherited from our creator. Things such as:
///
///     - pwd
///     - umask
///     - all file descriptors
///       - stdin, stdout & stderr should be closed (we don't know from or to where they may have
///         been redirected), and re-opened to locations appropriate to this daemon
///       - any other file descriptors should be closed; this process can then re-open any that
///         it needs for its work
///
/// In the end, the problem turned out not to be the daeomonize crate, but rather a more subtle
/// issue with the interaction between forking & the tokio runtime-- tokio will spin-up a thread
/// pool, and threads do not mix well with `fork()'. The trick is to fork this process *before*
/// starting-up the tokio runtime. In any event, I learned a lot about process management & wound up
/// choosing to do a few things differently.
///
/// <http://www.steve.org.uk/Reference/Unix/faq_2.html>
/// <https://en.wikipedia.org/wiki/SIGHUP>
/// <http://www.enderunix.org/docs/eng/daemon.php>

fn daemonize() -> Result {
    use std::os::unix::ffi::OsStringExt;

    unsafe {
        // Removing ourselves from from this process' controlling terminal's job control (if any).
        // Begin by forking; this does a few things:
        //
        // 1. returns control to the shell invoking us, if any
        // 2. guarantees that the child is not a process group leader
        let pid = fork();
        if pid < 0 {
            return Err(Error::Fork {
                errno: errno(),
                back: Backtrace::generate(),
            });
        } else if pid != 0 {
            // We are the parent process-- exit.
            exit(0);
        }

        // In the last step, we said we wanted to be sure we are not a process group leader. That
        // is because this call will fail if we do. It will create a new session, with us as
        // session (and process) group leader.
        setsid();

        // Since controlling terminals are associated with sessions, we now have no controlling tty
        // (so no job control, no SIGHUP when cleaning up that terminal, &c). We now fork again
        // and let our parent (the session group leader) exit; this means that this process can
        // never regain a controlling tty.
        let pid = fork();
        if pid < 0 {
            return Err(Error::Fork {
                errno: errno(),
                back: Backtrace::generate(),
            });
        } else if pid != 0 {
            // We are the parent process-- exit.
            exit(0);
        }

        // We next change the present working directory to avoid keeping the present one in
        // use. `mppopmd' can run pretty much anywhere, so /tmp is as good a place as any.
        std::env::set_current_dir("/tmp")?;

        umask(0);

        // Close all file descriptors ("nuke 'em from orbit-- it's the only way to be sure")...
        let mut i = getdtablesize() - 1;
        while i > -1 {
            close(i);
            i -= 1;
        }
        // and re-open stdin, stdout & stderr all redirected to /dev/null. `i' will be zero, since
        // "The file descriptor returned by a successful call will be the lowest-numbered file
        // descriptor not currently open for the process"...
        i = open(b"/dev/null\0" as *const [u8; 10] as _, libc::O_RDWR);
        // and these two will be 1 & 2 for the same reason.
        dup(i);
        dup(i);

        let pth: PathBuf = [LOCALSTATEDIR, "run", "mppopmd.pid"].iter().collect();
        let pth_c = CString::new(pth.into_os_string().into_vec())?;
        let fd = open(
            pth_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
            0o640,
        );
        if -1 == fd {
            return Err(Error::OpenLockFile {
                errno: errno(),
                back: Backtrace::generate(),
            });
        }
        if lockf(fd, F_TLOCK, 0) < 0 {
            return Err(Error::LockFile {
                errno: errno(),
                back: Backtrace::generate(),
            });
        }

        // "File locks are released as soon as the process holding the locks closes some file
        // descriptor for the file"-- just leave `fd' until this process terminates.

        let pid = getpid();
        let pid_buf = format!("{}", pid).into_bytes();
        let pid_length = pid_buf.len();
        let pid_c = CString::new(pid_buf)?;
        if write(fd, pid_c.as_ptr() as *const libc::c_void, pid_length) < pid_length as isize {
            return Err(Error::WritePid {
                errno: errno(),
                back: Backtrace::generate(),
            });
        }
    }

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                         The Big Kahuna                                         //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Entry point for `mppopmd'.
///
/// Do *not* use the #[tokio::main] attribute here! If this program is asked to daemonize (the usual
/// case), we will fork after tokio has started its thread pool, with disasterous consequences.
/// Instead, stay synchronous until we've daemonized (or figured out that we don't need to), and
/// only then fire-up the tokio runtime.
fn main() -> Result {
    use mpdpopm::vars::{AUTHOR, SYSCONFDIR, VERSION};

    let matches = App::new("mppopmd")
        .version(VERSION)
        .author(AUTHOR)
        .about("mpd + POPM")
        .long_about(
            "
`mppopmd' is a companion daemon for `mpd' that maintains playcounts & ratings,
as well as implementing some handy functions. It maintains ratings & playcounts in the sticker
database, but it allows you to keep that information in your tags, as well, by invoking external
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
    // and it's not there, that's fine: we just proceed with a default configuration values. But if
    // they explicitly named a configuration file, and it's not there, they presumably want to know
    // about that.
    let cfgpth = matches.value_of("config").context(NoConfigArg {})?;
    let cfg = match std::fs::read_to_string(cfgpth) {
        // The config file (defaulted or not) existed & we were able to read its contents-- parse
        // em!
        Ok(text) => config::from_str(&text)?,
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

    // `--verbose' & `--debug' work as follows: if `--debug' is present, log at level Trace, no
    // matter what. Else, if `--verbose' is present, log at level Debug. Else, log at level Info.
    let lf = match (matches.is_present("verbose"), matches.is_present("debug")) {
        (_, true) => LevelFilter::Trace,
        (true, false) => LevelFilter::Debug,
        _ => LevelFilter::Info,
    };

    // If we're not running as a daemon...
    if matches.is_present("no-daemon") {
        // log to stdout & let our caller redirect that.
        let app = ConsoleAppender::builder()
            .target(Target::Stdout)
            .encoder(Box::new(PatternEncoder::new("[{d}][{M}] {m}{n}")))
            .build();
        let lcfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(app)))
            .build(Root::builder().appender("stdout").build(lf))
            .unwrap();
        log4rs::init_config(lcfg)?;
    } else {
        // Else, daemonize this process now, before we spin-up the Tokio runtime.
        daemonize()?;
        // Also, log to file.
        let app = FileAppender::builder()
            .encoder(Box::new(PatternEncoder::new("{d}|{M}|{f}|{l}| {m}{n}")))
            .build(&cfg.log)
            .unwrap();
        let lcfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("logfile", Box::new(app)))
            .build(Root::builder().appender("logfile").build(lf))
            .unwrap();
        log4rs::init_config(lcfg)?;
    }

    // One way or another, we are now the "final" process. Announce ourselves...
    info!("mppopmd {} logging at level {:#?}.", VERSION, lf);
    // spin-up the Tokio runtime...
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    // and invoke `mpdpopm'. I can't figure out how to get Rust to map from Result<(),
    // mpdpopm::Error> to Result<(), Error>, so I'm stuck with this ugly match statement:
    match rt.block_on(mpdpopm(cfg)) {
        Ok(_) => Ok(()),
        Err(err) => Err(Error::from(err)),
    }
}
