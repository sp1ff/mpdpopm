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
use mpdpopm::mpdpopm;
use mpdpopm::vars::LOCALSTATEDIR;

use backtrace::Backtrace;
use clap::{value_parser, Arg, ArgAction, Command};
use errno::errno;
use lazy_static::lazy_static;
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

use std::{ffi::CString, fmt, path::PathBuf};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                 mppopmd application Error type                                 //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[non_exhaustive]
pub enum Error {
    NoConfigArg,
    NoConfig {
        config: std::path::PathBuf,
        cause: std::io::Error,
    },
    Fork {
        errno: errno::Errno,
        back: Backtrace,
    },
    PathContainsNull {
        back: Backtrace,
    },
    OpenLockFile {
        errno: errno::Errno,
        back: Backtrace,
    },
    LockFile {
        errno: errno::Errno,
        back: Backtrace,
    },
    WritePid {
        errno: errno::Errno,
        back: Backtrace,
    },
    Config {
        source: crate::config::Error,
        back: Backtrace,
    },
    Logging {
        source: log::SetLoggerError,
        back: Backtrace,
    },
    MpdPopm {
        source: mpdpopm::Error,
        back: Backtrace,
    },
}

impl std::fmt::Display for Error {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::NoConfigArg => write!(f, "No configuration file given"),
            Error::NoConfig { config, cause } => {
                write!(f, "Configuration error ({:?}): {}", config, cause)
            }
            Error::Fork { errno, back: _ } => write!(f, "When forking, got errno {}", errno),
            Error::PathContainsNull { back: _ } => write!(f, "Path contains a null character"),
            Error::OpenLockFile { errno, back: _ } => {
                write!(f, "While opening lock file, got errno {}", errno)
            }
            Error::LockFile { errno, back: _ } => {
                write!(f, "While locking the lock file, got errno {}", errno)
            }
            Error::WritePid { errno, back: _ } => {
                write!(f, "While writing pid file, got errno {}", errno)
            }
            Error::Config { source, back: _ } => write!(f, "Configuration error: {}", source),
            Error::Logging { source, back: _ } => write!(f, "Logging error: {}", source),
            Error::MpdPopm { source, back: _ } => write!(f, "mpdpopm error: {}", source),
            _ => write!(f, "Unknown mppopmd error"),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

type Result = std::result::Result<(), Error>;

lazy_static! {
    static ref DEF_CONF: String = format!("{}/mppopmd.conf", mpdpopm::vars::SYSCONFDIR);
}

/// Make this process into a daemon
///
/// I first tried to use the [daemonize](https://docs.rs/daemonize) crate, without success. Perhaps
/// I wasn't using it correctly. In any event, both to debug the issue(s) and for my own
/// edification, I decided to hand-code the daemonization process.
///
/// After spending a bit of time digging around the world of TTYs, process groups & sessions, I'm
/// beginning to understand what "daemon" means and how to create one. The first step is to
/// dissassociate this process from it's controlling terminal & make sure it cannot acquire a new
/// one. This, AFAIU, is to disconnect us from any job control associated with that terminal, and in
/// particular to prevent us from being disturbed when & if that terminal is closed (I'm still hazy
/// on the details, but at least the session leader (and perhaps it's descendants) will be sent a
/// `SIGHUP` in that eventuality).
///
/// After that, the rest of the work seems to consist of shedding all the things we (may have)
/// inherited from our creator. Things such as:
///
///   - pwd
///   - umask
///   - all file descriptors
///     - stdin, stdout & stderr should be closed (we don't know from or to where they may have
///       been redirected), and re-opened to locations appropriate to this daemon
///     - any other file descriptors should be closed; this process can then re-open any that
///       it needs for its work
///
/// In the end, the problem turned out not to be the daeomonize crate, but rather a more subtle
/// issue with the interaction between forking & the tokio runtime-- tokio will spin-up a thread
/// pool, and threads do not mix well with `fork()'. The trick is to fork this process *before*
/// starting-up the tokio runtime. In any event, I learned a lot about process management & wound up
/// choosing to do a few things differently.
///
/// References:
///
///   - <http://www.steve.org.uk/Reference/Unix/faq_2.html>
///   - <https://en.wikipedia.org/wiki/SIGHUP>
///   - <http://www.enderunix.org/docs/eng/daemon.php>

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
                back: Backtrace::new(),
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
                back: Backtrace::new(),
            });
        } else if pid != 0 {
            // We are the parent process-- exit.
            exit(0);
        }

        // We next change the present working directory to avoid keeping the present one in
        // use. `mppopmd' can run pretty much anywhere, so /tmp is as good a place as any.
        std::env::set_current_dir("/tmp").unwrap();

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
        let pth_c = CString::new(pth.into_os_string().into_vec()).unwrap();
        let fd = open(
            pth_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC,
            0o640,
        );
        if -1 == fd {
            return Err(Error::OpenLockFile {
                errno: errno(),
                back: Backtrace::new(),
            });
        }
        if lockf(fd, F_TLOCK, 0) < 0 {
            return Err(Error::LockFile {
                errno: errno(),
                back: Backtrace::new(),
            });
        }

        // "File locks are released as soon as the process holding the locks closes some file
        // descriptor for the file"-- just leave `fd' until this process terminates.

        let pid = getpid();
        let pid_buf = format!("{}", pid).into_bytes();
        let pid_length = pid_buf.len();
        let pid_c = CString::new(pid_buf).unwrap();
        if write(fd, pid_c.as_ptr() as *const libc::c_void, pid_length) < pid_length as isize {
            return Err(Error::WritePid {
                errno: errno(),
                back: Backtrace::new(),
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
    use mpdpopm::vars::{AUTHOR, VERSION};

    let matches = Command::new("mppopmd")
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
            Arg::new("no-daemon")
                .short('F')
                .long("no-daemon")
                .num_args(0)
                .action(ArgAction::SetTrue)
                .help("do not daemonize; remain in foreground"),
        )
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .num_args(1)
                .value_parser(value_parser!(PathBuf))
                .default_value(DEF_CONF.as_str())
                .help("path to configuration file"),
        )
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
        .get_matches();

    // Handling the configuration file is a little touchy; if the user simply accepted the default,
    // and it's not there, that's fine: we just proceed with a default configuration values. But if
    // they explicitly named a configuration file, and it's not there, they presumably want to know
    // about that.
    let cfgpth = matches
        .get_one::<PathBuf>("config")
        .ok_or_else(|| Error::NoConfigArg {})?;
    let cfg = match std::fs::read_to_string(cfgpth) {
        // The config file (defaulted or not) existed & we were able to read its contents-- parse
        // em!
        Ok(text) => config::from_str(&text).map_err(|err| Error::Config {
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

    // `--verbose' & `--debug' work as follows: if `--debug' is present, log at level Trace, no
    // matter what. Else, if `--verbose' is present, log at level Debug. Else, log at level Info.
    let lf = match (matches.get_flag("verbose"), matches.get_flag("debug")) {
        (_, true) => LevelFilter::Trace,
        (true, false) => LevelFilter::Debug,
        _ => LevelFilter::Info,
    };

    // If we're not running as a daemon...
    if matches.get_flag("no-daemon") {
        // log to stdout & let our caller redirect that.
        let app = ConsoleAppender::builder()
            .target(Target::Stdout)
            .encoder(Box::new(PatternEncoder::new("[{d}][{M}] {m}{n}")))
            .build();
        let lcfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(app)))
            .build(Root::builder().appender("stdout").build(lf))
            .unwrap();
        log4rs::init_config(lcfg).map_err(|err| Error::Logging {
            source: err,
            back: Backtrace::new(),
        })?;
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
        log4rs::init_config(lcfg).map_err(|err| Error::Logging {
            source: err,
            back: Backtrace::new(),
        })?;
    }

    // One way or another, we are now the "final" process. Announce ourselves...
    info!("mppopmd {} logging at level {:#?}.", VERSION, lf);
    // spin-up the Tokio runtime...
    let rt = tokio::runtime::Runtime::new().unwrap();
    // and invoke `mpdpopm'.
    rt.block_on(mpdpopm(cfg)).map_err(|err| Error::MpdPopm {
        source: err,
        back: Backtrace::new(),
    })
}
