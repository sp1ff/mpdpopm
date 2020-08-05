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

//! playcounts -- managing play counts & lastplayed times
//!
//! # Introduction
//!
//! Play counts & last played timestamps are maintained so long as [`PlayState::update`] is called
//! regularly (every few seconds, say). For purposes of library maintenance, however, they can be
//! set explicitly:
//!
//! - setpc PLAYCOUNT( TRACK)?
//! - setlp LASTPLAYED( TRACK)?
//!

use crate::clients::{Client, PlayerStatus};
use crate::commands::{spawn, TaggedCommandFuture};
use crate::error_from;

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
    #[snafu(display("The current track can't be modified when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("The path `{}' cannot be converted to a UTF-8 string", pth.display()))]
    BadPath { pth: PathBuf },
}

error_from!(crate::clients::Error);
error_from!(crate::commands::Error);
error_from!(std::num::ParseFloatError);
error_from!(std::num::ParseIntError);
error_from!(std::time::SystemTimeError);

type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                      playcount operations                                      //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Retrieve the play count for a track
pub async fn get_play_count(
    client: &mut Client,
    sticker: &str,
    file: &str,
) -> Result<Option<usize>> {
    match client.get_sticker(file, sticker).await? {
        Some(text) => Ok(Some(text.parse::<usize>()?)),
        None => Ok(None),
    }
}

/// Set the play count for a track-- this will run the associated command, if any
pub async fn set_play_count<I: Iterator<Item = String>>(
    client: &mut Client,
    sticker: &str,
    file: &str,
    play_count: usize,
    cmd: &str,
    args: &mut I,
    music_dir: &str,
) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
    client
        .set_sticker(file, sticker, &format!("{}", play_count))
        .await?;

    if cmd.len() == 0 {
        return Ok(None);
    }

    let mut params: HashMap<String, String> = HashMap::new();
    let full_path: PathBuf = [music_dir, file].iter().collect();
    params.insert(
        "full-file".to_string(),
        full_path
            .to_str()
            .context(BadPath {
                pth: full_path.clone(),
            })?
            .to_string(),
    );
    params.insert("playcount".to_string(), format!("{}", play_count));

    Ok(Some(TaggedCommandFuture::pin(
        spawn(cmd, args, &params).await?,
        None, /* No need to update the DB */
    )))
}

/// Retrieve the last played timestamp for a track (seconds since Unix epoch)
pub async fn get_last_played(
    client: &mut Client,
    sticker: &str,
    file: &str,
) -> Result<Option<u64>> {
    match client.get_sticker(file, sticker).await? {
        Some(text) => Ok(Some(text.parse::<u64>()?)),
        None => Ok(None),
    }
}

/// Set the last played for a track
pub async fn set_last_played(
    client: &mut Client,
    sticker: &str,
    file: &str,
    last_played: u64,
) -> Result<()> {
    client
        .set_sticker(file, sticker, &format!("{}", last_played))
        .await?;
    Ok(())
}

#[cfg(test)]
/// Let's test these
mod pc_lc_tests {

    use super::*;
    use crate::clients::test_mock::Mock;

    /// "Smoke" tests for play counts & last played times
    #[tokio::test]
    async fn pc_smoke() {
        let mock = Box::new(Mock::new(&[
            ("sticker get song \"a\" \"pc\"", "sticker: pc=11\nOK\n"),
            (
                "sticker get song \"a\" \"pc\"",
                "ACK [50@0] {sticker} no such sticker\n",
            ),
            ("sticker get song \"a\" \"pc\"", "splat!"),
        ]));
        let mut cli = Client::new(mock).unwrap();

        assert_eq!(
            get_play_count(&mut cli, "pc", "a").await.unwrap().unwrap(),
            11
        );
        let val = get_play_count(&mut cli, "pc", "a").await.unwrap();
        assert!(val.is_none());
        get_play_count(&mut cli, "pc", "a").await.unwrap_err();
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           PlayState                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

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
    // TODO(sp1ff): Re-factor & unit test
    /// Poll the server-- update our status; maybe increment the current track's play count; the
    /// caller must arrange to have this method invoked periodically to keep our state fresh
    pub async fn update<I: Iterator<Item = String>>(
        &mut self,
        client: &mut Client,
        playcount_cmd: &str,
        playcount_cmd_args: &mut I,
        music_dir: &str,
    ) -> Result<Option<std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>>> {
        let new_stat = client.status().await?;

        match (&self.last_server_stat, &new_stat) {
            (PlayerStatus::Play(last), PlayerStatus::Play(curr))
            | (PlayerStatus::Pause(last), PlayerStatus::Play(curr))
            | (PlayerStatus::Play(last), PlayerStatus::Pause(curr))
            | (PlayerStatus::Pause(last), PlayerStatus::Pause(curr)) => {
                // Last we knew, we were playing, and we're playing now.
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
            (PlayerStatus::Stopped, PlayerStatus::Stopped) => (),
        }

        let fut = match &new_stat {
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
                    let file = curr.file.to_str().context(BadPath {
                        pth: PathBuf::from(curr.file.clone()),
                    })?;
                    let curr_pc =
                        match get_play_count(client, &self.playcount_sticker, file).await? {
                            Some(pc) => pc,
                            None => 0,
                        };
                    debug!("Current PC is {}.", curr_pc);
                    set_last_played(
                        client,
                        &self.lastplayed_sticker,
                        file,
                        SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)?
                            .as_secs(),
                    )
                    .await?;
                    self.incr_play_count = true;
                    set_play_count(
                        client,
                        &self.playcount_sticker,
                        file,
                        curr_pc + 1,
                        &playcount_cmd,
                        playcount_cmd_args,
                        &music_dir,
                    )
                    .await?
                } else {
                    None
                }
            }
            PlayerStatus::Pause(_) | PlayerStatus::Stopped => None,
        };

        self.last_server_stat = new_stat;
        Ok(fut) // No need to update the DB
    }
}
