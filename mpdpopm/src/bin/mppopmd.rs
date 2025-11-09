// Copyright (C) 2020-2025 Michael herstine <sp1ff@pobox.com>
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

use clap::{Arg, ArgAction, Command, value_parser};
use errno::errno;
use lazy_static::lazy_static;
use libc::{
    F_TLOCK, close, dup, exit, fork, getdtablesize, getpid, lockf, open, setsid, umask, write,
};
use snafu::{ResultExt, Snafu};
use tokio::sync::mpsc;
use tracing::{error, info, level_filters::LevelFilter};
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt::MakeWriter, layer::SubscriberExt};

use std::{
    ffi::CString,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                 mppopmd application Error type                                 //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum Error {
    #[snafu(display("No configuration file given"))]
    NoConfigArg,
    #[snafu(display("Configuration error ({config:?}): {cause}"))]
    NoConfig {
        config: std::path::PathBuf,
        cause: std::io::Error,
    },
    #[snafu(display("Filter error: {source}"))]
    Filter {
        source: tracing_subscriber::filter::FromEnvError,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("When forking, got errno {errno}"))]
    Fork {
        errno: errno::Errno,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Path contains a null character"))]
    PathContainsNull {
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("While opening lock file, got errno {errno}"))]
    OpenLockFile {
        errno: errno::Errno,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("While locking the lock file, got errno {errno}"))]
    LockFile {
        errno: errno::Errno,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("While writing pid file, got errno {errno}"))]
    WritePid {
        errno: errno::Errno,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Configuration error: {source}"))]
    Config {
        source: crate::config::Error,
    },
    #[snafu(display("mpdpopm error: {source}"))]
    MpdPopm {
        source: mpdpopm::Error,
    },
}

type Result = std::result::Result<(), Error>;

type StdResult<T, E> = std::result::Result<T, E>;

lazy_static! {
    static ref DEF_CONF: String = format!("{}/mppopmd.conf", mpdpopm::vars::SYSCONFDIR);
}

/// A tracing-compatible, "reopenable" log file
///
/// I need a thing that implements [MakeWriter] to hand-off to a tracing [Layer]. [MakeWriter], in
/// turn, returns a thing that implements [std::io::Write] that is valid for some lifetime 'a.
///
/// Now, [MakeWriter] is implemented on `Arc<W>` or `Mutex<W>` for any `W` that implements
/// [std::io::Write]. The problem is, `Arc<Mutex<W>>` does *not* implement [MakeWriter], even if `W`
/// implements [std::io::Write].
///
/// I could only see two approaches; hand the [Layer] a reference to the thing and keep a reference
/// for myself, and invoke a method on the thing in response to a `SIGHUP`, or hand the thing off to
/// the [Layer] in toto and use some side-band communications channel to tell it to re-open the file
/// in response to a `SIGHUP`.
///
/// I've for the moment gone with the latter since the former would require me to somehow have the
/// thing implement [std::io::Write] *and* be thread-safe.
struct LogFile {
    fd: Arc<Mutex<std::fs::File>>,
}

impl LogFile {
    /// Open a file at `pth`; return a [LogFile] instance along with the send side of a channel
    /// the caller can use to close & re-open the file.
    pub fn open<P: AsRef<Path>>(
        pth: P,
    ) -> StdResult<(LogFile, mpsc::Sender<PathBuf>), std::io::Error> {
        let (tx, rx) = mpsc::channel::<PathBuf>(1);
        let fd = OpenOptions::new()
            .create(true)
            .append(true)
            .open(pth)
            .map(|fd| Arc::new(Mutex::new(fd)))?;
        tokio::spawn(LogFile::rehup(fd.clone(), rx));
        Ok((LogFile { fd }, tx))
    }
    /// Close & re-open the file
    async fn rehup(fd: Arc<Mutex<std::fs::File>>, mut rx: mpsc::Receiver<PathBuf>) {
        while let Some(ref pbuf) = rx.recv().await {
            match OpenOptions::new().create(true).append(true).open(pbuf) {
                Ok(f) => *fd.lock().unwrap() = f,
                Err(err) => error!("Failed to open {:?} ({}).", pbuf, err),
            }
        }
    }
}

pub struct MyMutexGuardWriter<'a>(MutexGuard<'a, std::fs::File>);

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = MyMutexGuardWriter<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        MyMutexGuardWriter(self.fd.lock().expect("lock poisoned"))
    }
}

impl io::Write for MyMutexGuardWriter<'_> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.0.write_vectored(bufs)
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }

    #[inline]
    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> io::Result<()> {
        self.0.write_fmt(fmt)
    }
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
            return Err(ForkSnafu { errno: errno() }.build());
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
            return Err(ForkSnafu { errno: errno() }.build());
        } else if pid != 0 {
            // We are the parent process-- exit.
            exit(0);
        }

        // We next change the present working directory to avoid keeping the present one in
        // use. `mppopmd' can run pretty much anywhere, so /tmp is as good a place as any.
        std::env::set_current_dir("/tmp").unwrap(); // A little unhappy about hard-coding that, but
        // if /tmp doesn't exist I expect few things will work.

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
            return Err(OpenLockFileSnafu { errno: errno() }.build());
        }
        if lockf(fd, F_TLOCK, 0) < 0 {
            return Err(LockFileSnafu { errno: errno() }.build());
        }

        // "File locks are released as soon as the process holding the locks closes some file
        // descriptor for the file"-- just leave `fd' until this process terminates.

        let pid = getpid();
        let pid_buf = format!("{}", pid).into_bytes();
        let pid_length = pid_buf.len();
        let pid_c = CString::new(pid_buf).unwrap();
        if write(fd, pid_c.as_ptr() as *const libc::c_void, pid_length) < pid_length as isize {
            return Err(WritePidSnafu { errno: errno() }.build());
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
        Ok(text) => config::from_str(&text).context(ConfigSnafu)?,
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
        (_, true) => LevelFilter::TRACE,
        (true, false) => LevelFilter::DEBUG,
        _ => LevelFilter::INFO,
    };

    let filter = EnvFilter::builder()
        .with_default_directive(lf.into())
        .from_env()
        .context(FilterSnafu)?;

    let (formatter, reopen): (
        Box<dyn Layer<Registry> + Send + Sync>,
        Option<tokio::sync::mpsc::Sender<PathBuf>>,
    ) = if matches.get_flag("no-daemon") {
        // If we're not running as a daemon, just log to stdout & let our caller redirect if they
        // want to.
        (
            Box::new(
                tracing_subscriber::fmt::Layer::default()
                    .compact()
                    .with_writer(io::stdout),
            ),
            None,
        )
    } else {
        // Else, daemonize this process now, before we spin-up the Tokio runtime...
        daemonize()?;
        let (log_file, tx) = LogFile::open(&cfg.log).unwrap();
        (
            Box::new(
                tracing_subscriber::fmt::Layer::default()
                    .compact()
                    .with_ansi(false)
                    .with_writer(log_file),
            ),
            Some(tx),
        )
    };
    tracing::subscriber::set_global_default(Registry::default().with(formatter).with(filter))
        .unwrap();

    // One way or another, we are now the "final" process. Announce ourselves...
    info!("mppopmd {VERSION} logging at level {lf:#?}.");
    // spin-up the Tokio runtime...
    let rt = tokio::runtime::Runtime::new().unwrap();
    // and invoke `mpdpopm'.
    rt.block_on(mpdpopm(cfg, reopen))
        .context(MpdPopmSnafu)
}
