// Copyright (C) 2020 Michael Herstine <sp1ff@pobox.com>
//
// This file is part of mpdpopm.
//
// mpdpopm is free software: you can redistribute it and/or modify it under the terms of the GNU General
// Public License as published by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// mpdpopm is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the
// implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU General
// Public License for more details.
//
// You should have received a copy of the GNU General Public License along with mpdpopm.  If not, see
// <http://www.gnu.org/licenses/>.

use crate::replstrings::process_replacements;
use crate::clients::{Client, PlayerStatus};

use futures::{
    stream::{FuturesUnordered},
};
use log::{debug, info};
use snafu::{Backtrace, GenerateBacktrace, OptionExt, Snafu};

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           Error type                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    #[snafu(display("The path `{}' cannot be converted to a UTF-8 string", pth.display()))]
    BadPath {
        pth: PathBuf
    },
}

////////////////////////////////////////////////////////////////////////////////////////////////////

// TODO(sp1ff): re-factor this into one place
macro_rules! error_from {
    ($t:ty) => (
        impl std::convert::From<$t> for Error {
            fn from(err: $t) -> Self {
                Error::Other{ cause: Box::new(err), back: Backtrace::generate() }
            }
        }
    )
}

error_from!(crate::clients::Error);
error_from!(crate::replstrings::Error);
error_from!(std::num::ParseFloatError);
error_from!(std::num::ParseIntError);
error_from!(std::time::SystemTimeError);

// TODO(sp1ff): move this to one location
pub type PinnedCmdFut = std::pin::Pin<
    Box<dyn futures::future::Future<Output = tokio::io::Result<std::process::Output>>>,
>;

/// Current server state in terms of the play status (stopped/paused/playing, current track, elapsed
/// time in current track, &c)
#[derive(Debug)]
pub struct PlayState {
    /// Last known server status
    last_server_stat: PlayerStatus,
    /// Sticker under which to store play counts
    playcount_sticker: String,
    /// Sticker under which to store the last played timestamp
    lastplayed_sticker: String,
    /// true iff we have already incremented the last known track's playcount
    incr_play_count: bool,
    /// Percentage threshold, expressed as a number between zero & one, for considering a song to
    /// have been played
    played_thresh: f64,
}

impl PlayState {
    /// Create a new PlayState instance; async because it will reach out to the mpd server
    /// to get current status.
    pub async fn new(
        client: &mut Client,
        playcount_sticker: &str,
        lastplayed_sticker: &str,
        played_thresh: f64,
    ) -> std::result::Result<PlayState, crate::clients::Error> {
        Ok(PlayState {
            last_server_stat: client.status().await?,
            playcount_sticker: playcount_sticker.to_string(),
            lastplayed_sticker: lastplayed_sticker.to_string(),
            incr_play_count: false,
            played_thresh: played_thresh,
        })
    }
    /// Retrieve a copy of the last known player status
    pub fn last_status(&self) -> PlayerStatus {
        self.last_server_stat.clone()
    }
    // TODO(sp1ff): Ugh-- needs re-factor
    /// Poll the server-- update our status; maybe increment the current track's play count; the
    /// caller must arrange to have this method invoked periodically to keep our state fresh
    pub async fn update(
        &mut self,
        client: &mut Client,
        playcount_cmd: &str,
        playcount_cmd_args: &Vec<String>,
        music_dir: &PathBuf,
        cmds: &mut FuturesUnordered<PinnedCmdFut>,
    ) -> std::result::Result<(), Error> {
        let new_stat = client.status().await?;


        match (&self.last_server_stat, &new_stat) {
            (PlayerStatus::Play(last),  PlayerStatus::Play(curr))  |
            (PlayerStatus::Pause(last), PlayerStatus::Play(curr))  |
            (PlayerStatus::Play(last),  PlayerStatus::Pause(curr)) |
            (PlayerStatus::Pause(last), PlayerStatus::Pause(curr)) => {
                // Last we knew, we were playing, and we're playing now.
                if last.songid != curr.songid {
                    debug!("New songid-- resetting PC incremented flag.");
                    self.incr_play_count = false;
                } else if last.elapsed > curr.elapsed &&
                    self.incr_play_count &&
                    curr.elapsed / curr.duration <= 0.1
                {
                    debug!("Re-play-- resetting PC incremented flag.");
                    self.incr_play_count = false;
                }
            }
            (PlayerStatus::Stopped,  PlayerStatus::Play(_))  |
            (PlayerStatus::Stopped,  PlayerStatus::Pause(_)) |
            (PlayerStatus::Pause(_), PlayerStatus::Stopped)  |
            (PlayerStatus::Play(_),  PlayerStatus::Stopped) => {
                self.incr_play_count = false;
            }
            (PlayerStatus::Stopped, PlayerStatus::Stopped) => ()
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
                    let file = curr.file.to_str()
                        .context(BadPath { pth: PathBuf::from(curr.file.clone()) })?;
                    let curr_pc = match client.get_sticker(file, &self.playcount_sticker).await? {
                        Some(text) => text.parse::<u64>()?,
                        None => 0,
                    };
                    self.incr_play_count = true;
                    debug!("Current PC is {}.", curr_pc);
                    client
                        .set_sticker(file, &self.playcount_sticker, &format!("{}", curr_pc + 1))
                        .await?;
                    client
                        .set_sticker(file, &self.lastplayed_sticker,
                                     &format!("{}", SystemTime::now()
                                              .duration_since(SystemTime::UNIX_EPOCH)?.as_secs()))
                        .await?;

                    if playcount_cmd.len() != 0 {
                        let mut params: HashMap<String, String> = HashMap::new();
                        let mut path = music_dir.clone();
                        path.push(curr.file.clone());
                        params.insert(
                            "full-file".to_string(),
                            path.to_str().context(BadPath { pth: path.clone() })?.to_string());
                        params.insert("playcount".to_string(), format!("{}", curr_pc + 1));

                        let cmd = process_replacements(playcount_cmd, &params)?;
                        let args: std::result::Result<Vec<_>, _> = playcount_cmd_args
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
