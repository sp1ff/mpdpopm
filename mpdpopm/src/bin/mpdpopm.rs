#![recursion_limit = "512"]
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

use mpdpopm::client::{Client, IdleClient, IdleSubSystem, PlayerStatus};
use mpdpopm::ratings::do_rating;
use mpdpopm::{process_replacements, PinnedCmdFut};

use clap::{App, Arg};
use futures::{
    future::FutureExt,
    pin_mut, select,
    stream::{FuturesUnordered, StreamExt},
};
use log::{debug, info, warn, LevelFilter};
use log4rs::{
    append::{
        console::{ConsoleAppender, Target},
        file::FileAppender,
    },
    config::{Appender, Root},
    encode::pattern::PatternEncoder,
};
use serde::{Deserialize, Serialize};
use tokio::{
    signal,
    signal::unix::{signal, SignalKind},
    time::{delay_for, Duration},
};

use std::{collections::HashMap, fmt, fs::File, path::PathBuf};

/// An enumeration of `mpdpopm' errors
#[derive(Debug)]
pub enum Cause {
    /// An error occurred in some sub-system (network I/O, or string formatting, e.g.)
    SubModule,
    /// Somehow got a message for a channel we don't support
    UnsupportedChannel(String),
    /// Non utf-8 path(?)
    BadPath(std::path::PathBuf),
}

/// An Error that occurred somewhere in the `mpdpopm' program

/// This could be a direct result of something that happened in this module, or in a dependent
/// module. In the former case, [`Error::source`] will be [`None`]. In the latter, it will contain
/// the original [`std::error::Error`].
///
/// [`None`]: std::option::Option::None
#[derive(Debug)]
pub struct Error {
    /// Enumerated status code-- perhaps this is a holdover from my C++ days, but I've found that
    /// programmatic error-handling is facilitated by status codes, not text. Textual messages
    /// should be synthesized from other information only when it is time to present the error to a
    /// human (in a log file, say).
    cause: Cause,
    // This is an Option that may contain a Box containing something that implements
    // std::error::Error.  It is still unclear to me how this satisfies the lifetime bound in
    // std::error::Error::source, which additionally mandates that the boxed thing have 'static
    // lifetime. There is a discussion of this at
    // <https://users.rust-lang.org/t/what-does-it-mean-to-return-dyn-error-static/37619/6>,
    // but at the time of this writing, i cannot follow it.
    source: Option<Box<dyn std::error::Error>>,
}

impl Error {
    pub fn new(cause: Cause) -> Error {
        Error {
            cause: cause,
            source: None,
        }
    }
}

impl fmt::Display for Error {
    /// Format trait for an empty format, {}.
    ///
    /// I'm unsure of the conventions around implementing this method, but I've chosen to provide
    /// a one-line representation of the error suitable for writing to a log file or stderr.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.cause {
            Cause::SubModule => write!(f, "Cause: {:#?}", self.source),
            Cause::UnsupportedChannel(x) => write!(f, "Unknown channel {}", x),
            Cause::BadPath(x) => write!(f, "{:#?} cannot be represented as a String", x),
        }
    }
}

impl std::error::Error for Error {
    /// The lower-level source of this error, if any.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            // This is an Option that may contain a reference to something that implements
            // std::error::Error and has lifetime 'static. I'm still not sure what 'static means,
            // exactly, but at the time of this writing, I take it to mean a thing which can, if
            // needed, last for the program lifetime (e.g. it contains no references to anything
            // that itself does not have 'static lifetime)
            Some(bx) => Some(bx.as_ref()),
            None => None,
        }
    }
}

impl std::convert::From<mpdpopm::client::Error> for Error {
    fn from(err: mpdpopm::client::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<serde_lexpr::error::Error> for Error {
    fn from(err: serde_lexpr::error::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<mpdpopm::ReplacementStringError> for Error {
    fn from(err: mpdpopm::ReplacementStringError) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl std::convert::From<std::num::ParseIntError> for Error {
    fn from(err: std::num::ParseIntError) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

// TODO(sp1ff): Implement reading this from file; how to setup defaults, again?
#[derive(Serialize, Deserialize, Debug)]
struct Config {
    /// Location of log file
    log: PathBuf,
    // TODO(sp1ff): I think we need to run this on the same host-- use socket only?
    /// Host on which `mpd' is listening
    host: String,
    /// TCP port on which `mpd' is listening
    port: u16,
    // TODO(sp1ff): If I'm co-located, I can get this directly from the mpd daemon
    /// The `mpd' root music directory, relative to the host on which *this* daemon is running
    local_music_dir: PathBuf,
    /// Sticker name under which to store playcounts
    playcount_sticker: String,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    played_thresh: f64,
    /// The interval, in milliseconds, at which to poll `mpd' for the current state
    poll_interval_ms: u64,
    /// Command, with replacement parameters, to be run when a song's playcount is incremented
    playcount_command: String,
    /// Args, with replacement parameters, for the playcount command
    playcount_command_args: Vec<String>,
    /// Channel to setup for "rate" commands-- channel names must satisfy "[-a-zA-Z-9_.:]+"
    ratings_chan: String,
    /// Sticker under which to store song ratings, as a textual representation of a number in
    /// [0,255]
    rating_sticker: String,
    /// Command, with replacement parameters, to be run when a song is rated
    ratings_command: String,
    /// Args, with replacement parameters, for the ratings command
    ratings_command_args: Vec<String>,
}

impl Config {
    fn new() -> Config {
        // TODO(sp1ff): change these defaults to something more appropriate
        Config {
            log: PathBuf::from("/tmp/mpdpopm.log"),
            host: String::from("localhost"),
            port: 16600,
            local_music_dir: PathBuf::from("/mnt/Took-Hall/mp3"),
            playcount_sticker: String::from("unwoundstack.com:playcount"),
            played_thresh: 0.6,
            poll_interval_ms: 5000,
            playcount_command: String::new(),
            playcount_command_args: Vec::<String>::new(),
            ratings_chan: String::from("unwoundstack.com:ratings"),
            rating_sticker: String::from("unwoundstack.com:rating"),
            ratings_command: String::new(),
            ratings_command_args: Vec::<String>::new(),
        }
    }
}

// TODO(sp1ff): move this into it's own module?
/// Current server state in terms of the play status (stopped/paused/playing, current track, elapsed
/// time in current track, &c)
#[derive(Debug)]
struct PlayState {
    /// Last known server status
    last_server_stat: PlayerStatus,
    /// Sticker under which to store play counts
    playcount_sticker: String,
    /// true iff we have already incremented the last known track's playcount
    incr_play_count: bool,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    played_thresh: f64,
}

impl PlayState {
    /// Create a new PlayState instance; async because it will reach out to the mpd server
    /// to get current status.
    async fn new(
        client: &mut Client,
        playcount_sticker: &str,
        played_thresh: f64,
    ) -> Result<PlayState, mpdpopm::client::Error> {
        let server = client.status().await?;
        Ok(PlayState {
            last_server_stat: server,
            playcount_sticker: playcount_sticker.to_string(),
            incr_play_count: false,
            played_thresh: played_thresh,
        })
    }
    /// Retrieve a copy of the last known player status
    pub fn last_status(&self) -> PlayerStatus {
        self.last_server_stat.clone()
    }
    /// Poll the server-- update our status; maybe increment the current track's play count
    async fn update(
        &mut self,
        client: &mut Client,
        playcount_cmd: &str,
        playcount_cmd_args: &Vec<String>,
        music_dir: &PathBuf,
        cmds: &mut FuturesUnordered<PinnedCmdFut>,
    ) -> Result<(), Error> {
        let new_stat = client.status().await?;

        match (&self.last_server_stat, &new_stat) {
            (PlayerStatus::Play(last), PlayerStatus::Play(curr))
            | (PlayerStatus::Pause(last), PlayerStatus::Play(curr))
            | (PlayerStatus::Play(last), PlayerStatus::Pause(curr))
            | (PlayerStatus::Pause(last), PlayerStatus::Pause(curr)) => {
                if last.songid != curr.songid {
                    debug!("New songid-- resetting PC incremented flag.");
                    self.incr_play_count = false;
                } else if last.elapsed > curr.elapsed
                    && self.incr_play_count
                    && curr.elapsed / curr.duration <= 0.1
                {
                    debug!("Re-play-- resetting PC incremented flag.");
                    self.incr_play_count = false;
                }
            }
            (PlayerStatus::Stopped, PlayerStatus::Play(_))
            | (PlayerStatus::Stopped, PlayerStatus::Pause(_))
            | (PlayerStatus::Pause(_), PlayerStatus::Stopped)
            | (PlayerStatus::Play(_), PlayerStatus::Stopped) => {
                self.incr_play_count = false;
            }
            (PlayerStatus::Stopped, PlayerStatus::Stopped) => {}
        }

        match &new_stat {
            PlayerStatus::Play(curr) => {
                let pct = curr.played_pct();
                debug!("Updating status: {:.3}% complete.", 100.0 * pct);
                if !self.incr_play_count && pct >= self.played_thresh {
                    info!(
                        "Increment play count for '{}' (songid: {}) at {} played.",
                        curr.file.display(),
                        curr.songid,
                        curr.elapsed / curr.duration
                    );
                    let file = match curr.file.to_str() {
                        Some(text) => text,
                        None => {
                            // TODO(sp1ff): Ugh
                            return Err(Error::new(Cause::BadPath(curr.file.clone())));
                        }
                    };
                    let curr_pc = match client.get_sticker(file, &self.playcount_sticker).await? {
                        Some(text) => text.parse::<u64>()?,
                        None => 0,
                    };
                    self.incr_play_count = true;
                    debug!("Current PC is {}.", curr_pc);
                    client
                        .set_sticker(file, &self.playcount_sticker, &format!("{}", curr_pc + 1))
                        .await?;

                    if playcount_cmd.len() != 0 {
                        let mut params: HashMap<String, String> = HashMap::new();
                        let mut path = music_dir.clone();
                        path.push(curr.file.clone());
                        params.insert(
                            "full-file".to_string(),
                            match path.to_str() {
                                Some(s) => s.to_string(),
                                None => {
                                    // TODO(sp1ff): Ugh
                                    return Err(Error::new(Cause::BadPath(path)));
                                }
                            },
                        );
                        params.insert("playcount".to_string(), format!("{}", curr_pc + 1));

                        let cmd = process_replacements(playcount_cmd, &params)?;
                        let args: Result<Vec<_>, _> = playcount_cmd_args
                            .iter()
                            .map(|x| process_replacements(x, &params))
                            .collect();
                        match args {
                            Ok(a) => {
                                info!("Running playcount command: {} with args {:#?}", cmd, a);
                                cmds.push(Box::pin(
                                    tokio::process::Command::new(&cmd).args(a).output(),
                                ));
                            }
                            Err(err) => {
                                return Err(Error::from(err));
                            }
                        }
                    }
                }
            }
            PlayerStatus::Pause(_) | PlayerStatus::Stopped => {}
        }

        self.last_server_stat = new_stat;
        Ok(())
    }
}

// TODO(sp1ff): ugh
async fn mpdpopm(cfg: Config) -> std::result::Result<(), Error> {
    info!("mpdpopm {} beginning.", mpdpopm::vars::VERSION);

    // TODO(sp1ff): Ugh
    let host = cfg.host;
    let port = cfg.port;
    let music_dir = cfg.local_music_dir;
    let ratings_chan = cfg.ratings_chan;
    let rating_sticker = cfg.rating_sticker;
    let ratings_cmd = cfg.ratings_command;
    let ratings_cmd_args = cfg.ratings_command_args;
    let poll_interval_ms = cfg.poll_interval_ms;
    let playcount_sticker = cfg.playcount_sticker;
    let playcount_cmd = cfg.playcount_command;
    let playcount_cmd_args = cfg.playcount_command_args;
    let played_thresh = cfg.played_thresh;

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();

    let mut client = Client::connect(format!("{}:{}", host, port)).await?;
    let mut state = PlayState::new(&mut client, &playcount_sticker, played_thresh).await?;

    let mut idle_client = IdleClient::connect(format!("{}:{}", host, port)).await?;
    idle_client.subscribe(&ratings_chan).await?;

    let tick = delay_for(Duration::from_millis(poll_interval_ms)).fuse();

    let ctrl_c = signal::ctrl_c().fuse(); // <===
    let sighup = hup.recv().fuse();
    let sigkill = kill.recv().fuse();

    pin_mut!(ctrl_c, sighup, sigkill, tick);

    let mut cmds = FuturesUnordered::<PinnedCmdFut>::new();
    cmds.push(Box::pin(tokio::process::Command::new("pwd").output()));

    let mut done = false;
    while !done {
        debug!("selecting...");
        let mut msg_check_needed = false;
        {
            let mut idle = Box::pin(idle_client.idle().fuse()); // `idle_client' mutably borrowed here
            loop {
                select! {
                    // TODO(sp1ff): figure out why Ctrl-C handling breaks at some point
                    _ = ctrl_c => {
                        info!("got ctrl-C");
                        done = true;
                        break;
                    },
                    _ = sighup => {
                        info!("got SIGHUP");
                        done= true;
                        break;
                    },
                    _ = sigkill => {
                        info!("got SIGKILL");
                        done = true;
                        break;
                    },
                    next = cmds.next() => {
                        // tokio::io::Result<std::process::Output>
                        match next {
                            Some(res) => {
                                // TODO(sp1ff): implement me!
                                debug!("output status is {:#?}", res);
                            },
                            None => {
                                debug!("No more commands to process.");
                            }
                        }
                    },
                    _ = tick => {
                        tick.set(delay_for(Duration::from_millis(poll_interval_ms)).fuse());
                        state.update(&mut client, &playcount_cmd, &playcount_cmd_args, &music_dir,
                                     &mut cmds).await?;
                    },
                    res = idle => {
                        match res {
                            Ok(subsys) => {
                                debug!("subsystem {} changed", subsys);
                                if subsys == IdleSubSystem::Player {
                                    debug!("Updating status.");
                                    state.update(&mut client, &playcount_cmd, &playcount_cmd_args,
                                                 &music_dir, &mut cmds).await?;
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
            }
        } // `idle_client' mutable borrowed dropped here, which is important...

        // because it will be mutably borrowed again here:
        if msg_check_needed {
            let m = idle_client.get_messages().await?;
            for (chan, msgs) in &m {
                if chan == &ratings_chan {
                    match state.last_status() {
                        PlayerStatus::Stopped => {
                            warn!("Player is stopped-- can't rate current track.");
                        }
                        PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                            let file = curr.file;
                            for msg in msgs {
                                match do_rating(
                                    msg,
                                    &mut client,
                                    &rating_sticker,
                                    &ratings_cmd,
                                    &ratings_cmd_args,
                                    &music_dir,
                                    &file,
                                )
                                .await
                                {
                                    Ok(opt) => match opt {
                                        Some(fut) => cmds.push(fut),
                                        None => {}
                                    },
                                    Err(err) => warn!("{:#?}", err),
                                }
                            }
                        }
                    } // Close `match'
                } else {
                    return Err(Error::new(Cause::UnsupportedChannel(chan.to_string())));
                } // End `if'
            } // End `for'.
        } // End `if'
    } // End `while'.

    info!("mpdpopm exiting.");

    Ok(())
}

// Anything that implements std::process::Termination can be used as the return type for `main'. The
// std lib implements impl<E: Debug> Termination for Result<()undefined E> per
// <https://www.joshmcguigan.com/blog/custom-exit-status-codes-rust/>, so we're good.
#[tokio::main]
async fn main() -> Result<(), Error> {
    use mpdpopm::vars::{AUTHOR, VERSION};

    // TODO(sp1ff): have `clap' return 2 on failure to parse command line arguments
    let matches = App::new("mpdpopm")
        .version(VERSION)
        .author(AUTHOR)
        .about("mpd + POPM")
        .arg(
            Arg::with_name("no-daemon")
                .short("F")
                .help("Do not daemonize; remain in foreground"),
        )
        .arg(
            Arg::with_name("config")
                .short("c")
                .takes_value(true)
                .value_name("FILE")
                .help("path to configuration file"),
        )
        .get_matches();

    let config = match matches.value_of("config") {
        Some(f) => serde_lexpr::from_reader(File::open(f)?)?,
        None => Config::new(),
    };

    let daemonize = !matches.is_present("no-daemon");

    let _log_handle = if daemonize {
        let app = FileAppender::builder()
            .encoder(Box::new(PatternEncoder::new("{d}|{f}|{l}|{m}{n}")))
            .build(&config.log)
            .unwrap();
        let cfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("logfile", Box::new(app)))
            .build(Root::builder().appender("logfile").build(LevelFilter::Info))
            .unwrap();
        log4rs::init_config(cfg)
    } else {
        let app = ConsoleAppender::builder()
            .target(Target::Stdout)
            .encoder(Box::new(PatternEncoder::new("[{d}] {m}{n}")))
            .build();
        let cfg = log4rs::config::Config::builder()
            .appender(Appender::builder().build("stdout", Box::new(app)))
            .build(Root::builder().appender("stdout").build(LevelFilter::Debug))
            .unwrap();
        log4rs::init_config(cfg)
    };

    info!("logging configured.");

    // TODO(sp1ff): daemonize here, if `daemonize' is true

    mpdpopm(config).await
}
