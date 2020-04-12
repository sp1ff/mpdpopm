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

use mpdpopm::client::*;
use mpdpopm::ratings::*;
use mpdpopm::vars::VERSION;
use mpdpopm::{process_replacements, PinnedCmdFut};

use clap::{App, Arg};
use futures::{
    future::{Future, FutureExt},
    pin_mut, select,
    stream::{FuturesUnordered, StreamExt},
};
use log::{debug, info, LevelFilter};
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

use std::{collections::HashMap, fs::File, path::PathBuf};

// TODO(sp1ff): clean this up
// #[derive(Debug, Clone)]
// struct AppError {
//     msg: String,
// }

// impl From<std::io::Error> for AppError {
//     fn from(err: std::io::Error) -> Self {
//         AppError {
//             msg: format!("{:#?}", err),
//         }
//     }
// }

// impl From<std::string::FromUtf8Error> for AppError {
//     fn from(err: std::string::FromUtf8Error) -> Self {
//         AppError {
//             msg: format!("{:#?}", err),
//         }
//     }
// }

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
        // TODO(sp1ff): change this to something more appropriate
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

/// Current server state in terms of the play status (stopped/paused/playing, current track, elapsed
/// time in current track, &c)
#[derive(Debug)]
struct PlayState {
    /// Last known server status
    last_server_stat: ServerStatus,
    /// Sticker under which to store play counts
    playcount_sticker: String,
    /// true iff we have already incremented the last known track's playcount
    incr_play_count: bool,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    played_thresh: f64,
}

impl PlayState {
    async fn new(
        client: &mut Client,
        playcount_sticker: &str,
        played_thresh: f64,
    ) -> Result<PlayState, MpdClientError> {
        let server = client.status().await?;
        Ok(PlayState {
            last_server_stat: server,
            playcount_sticker: playcount_sticker.to_string(),
            incr_play_count: false,
            played_thresh: played_thresh,
        })
    }
    pub fn last_status(&self) -> ServerStatus {
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
    ) -> Result<(), MpdClientError> {
        let new_stat = client.status().await?;

        // If the song has changed, re-set our "incremented" flag
        if new_stat.songid != self.last_server_stat.songid {
            self.incr_play_count = false;
        // Else, *if* we've incremented the play count for the current track, and if we've
        // gone back in time to the beginning, we're eligible to increment it again.
        } else if self.last_server_stat.elapsed > new_stat.elapsed
            && self.incr_play_count
            && new_stat.elapsed / new_stat.duration <= 0.1
        {
            self.incr_play_count = false;
        }

        if new_stat.state == PlayerState::Play
            && !self.incr_play_count
            && new_stat.elapsed / new_stat.duration >= self.played_thresh
        {
            info!(
                "Increment play count for '{}' (songid: {}) at {} played.",
                new_stat.file.display(),
                new_stat.songid,
                new_stat.elapsed / new_stat.duration
            );
            let file = match self.last_server_stat.file.to_str() {
                Some(text) => text,
                None => {
                    return Err(MpdClientError::new(&format!(
                        "{} can't be represented as a string",
                        new_stat.file.display()
                    )))
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
                path.push(self.last_server_stat.file.clone());
                params.insert(
                    "full-file".to_string(),
                    match path.to_str() {
                        Some(s) => s.to_string(),
                        None => {
                            return Err(MpdClientError::new(&format!(
                                "Couldn't represent {:#?} as UTF-8",
                                path
                            )));
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
                        return Err(MpdClientError::new(&format!(
                            "error processing cmd args: {}",
                            err
                        )));
                    }
                }
            }
        }

        self.last_server_stat = new_stat;
        Ok(())
    }
}

async fn mpdpopm(
    host: &str,
    port: u16,
    music_dir: &PathBuf,
    ratings_chan: &str,
    rating_sticker: &str,
    ratings_cmd: &str,
    ratings_cmd_args: &Vec<String>,
    poll_interval_ms: u64,
    playcount_sticker: &str,
    playcount_cmd: &str,
    playcount_cmd_args: &Vec<String>,
    played_thresh: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("mpdpopm {} beginning.", VERSION);

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();

    let mut client = Client::connect(format!("{}:{}", host, port)).await?;
    let mut state = PlayState::new(&mut client, playcount_sticker, played_thresh).await?;

    let mut idle_client = IdleClient::connect(format!("{}:{}", host, port)).await?;
    idle_client.subscribe(ratings_chan).await?;

    let tick = delay_for(Duration::from_millis(poll_interval_ms)).fuse();

    let ctrl_c = signal::ctrl_c().fuse(); // <===
    let sighup = hup.recv().fuse();
    let sigkill = kill.recv().fuse();

    pin_mut!(ctrl_c, sighup, sigkill, tick);

    // TODO(sp1ff): figure out how to make this work:
    let x: std::pin::Pin<Box<dyn Future<Output = tokio::io::Result<std::process::Output>>>> =
        Box::pin(tokio::process::Command::new("pwd").output());
    let mut cmds/*: FuturesUnordered<
        std::pin::Pin<Box<dyn Future<Output = tokio::io::Result<std::process::Output>>>>,
        >*/ = FuturesUnordered::<
                PinnedCmdFut
                // std::pin::Pin<Box<dyn Future<Output = tokio::io::Result<std::process::Output>>>>,
    >::new();
    cmds.push(x);

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
                        debug!("Updating status.");
                        tick.set(delay_for(Duration::from_millis(poll_interval_ms)).fuse());
                        state.update(&mut client, &playcount_cmd, playcount_cmd_args, music_dir,
                                     &mut cmds).await?;
                    },
                    res = idle => {
                        match res {
                            Ok(subsys) => {
                                debug!("subsystem {} changed", subsys);
                                if subsys == IdleSubSystem::Player {
                                    debug!("Updating status.");
                                    state.update(&mut client, &playcount_cmd, playcount_cmd_args,
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
            debug!("Checking messages.");
            check_messages(
                &mut client,
                &mut idle_client,
                &ratings_chan,
                &rating_sticker,
                &ratings_cmd,
                &ratings_cmd_args,
                music_dir,
                &state.last_status(),
                &mut cmds,
            )
            .await?;
        }
    }

    info!("mpdpopm exiting.");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mpdpopm::vars::AUTHOR;

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
            // TODO(sp1ff): make this configurable
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

    mpdpopm(
        &config.host,
        config.port,
        &config.local_music_dir,
        &config.ratings_chan,
        &config.rating_sticker,
        &config.ratings_command,
        &config.ratings_command_args,
        config.poll_interval_ms,
        &config.playcount_sticker,
        &config.playcount_command,
        &config.playcount_command_args,
        config.played_thresh,
    )
    .await
}
