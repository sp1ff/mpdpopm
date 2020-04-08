#![recursion_limit = "256"]
//! # mpdpopm
//!
//! TODO(sp1ff): Document me!

use mpdpopm::client::*;

use clap::{App, Arg};
use futures::future::FutureExt;
use futures::{pin_mut, select};
use log::{debug, info, LevelFilter};
use log4rs::append::console::{ConsoleAppender, Target};
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Root};
use log4rs::encode::pattern::PatternEncoder;
use serde::{Deserialize, Serialize};
use std::fs::File;
use tokio::signal;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{delay_for, Duration};

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

#[derive(Serialize, Deserialize, Debug)]
struct Config {
    log: String,
    host: String,
    port: u16,
    ratings_chan: String,
    ratings_sticker: String,
    playcount_sticker: String,
    popm_email: String,
}

impl Config {
    fn new() -> Config {
        // TODO(sp1ff): change this to something more appropriate
        Config {
            log: String::from("/tmp/mpdpopm.log"),
            host: String::from("localhost"),
            port: 16600,
            ratings_chan: String::from("unwoundstack.com:ratings"),
            ratings_sticker: String::from("unwoundstack.com:rating"),
            playcount_sticker: String::from("unwoundstack.com:playcount"),
            popm_email: String::from("sp1ff@pobox.com"),
        }
    }
}

async fn mpdpopm(
    host: &str,
    port: u16,
    ratings_chan: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("mpdpopm beginning.");

    let mut hup = signal(SignalKind::hangup()).unwrap();
    let mut kill = signal(SignalKind::terminate()).unwrap();

    let mut client = Client::connect(format!("{}:{}", host, port)).await?;
    let mut curr_stat = client.status().await?;

    let mut idle_client = IdleClient::connect(format!("{}:{}", host, port)).await?;
    idle_client.subscribe(ratings_chan).await?;

    let tick = delay_for(Duration::from_millis(5000)).fuse();

    let ctrl_c = signal::ctrl_c().fuse(); // <===
    let sighup = hup.recv().fuse();
    let sigkill = kill.recv().fuse();

    pin_mut!(ctrl_c, sighup, sigkill, tick);

    let mut done = false;
    while !done {
        debug!("selecting...");
        {
            let mut idle = Box::pin(idle_client.idle().fuse());
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
                        info!("SIGKILL!");
                        done = true;
                        break;
                    },
                    _ = tick => {
                        debug!("Updating status.");
                        tick.set(delay_for(Duration::from_millis(5000)).fuse());
                        curr_stat = client.status().await?;
                    },
                    res = idle => {
                        match res {
                            Ok(subsys) => {
                                debug!("subsystem {} changed", subsys);
                                if subsys == IdleSubSystem::Player {
                                    debug!("Updating status.");
                                    curr_stat = client.status().await?;
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
        }
    }

    info!("mpdpopm exiting.");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mpdpopm::vars::AUTHOR;
    use mpdpopm::vars::VERSION;

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

    mpdpopm(&config.host, config.port, &config.ratings_chan).await
}
