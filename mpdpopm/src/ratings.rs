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

//! Logic for rating MPD tracks.
//!
//! # Introduction
//!
//! This module contains types implementing a basic rating functionality for `mpd'.
//!
//! # Discussion
//!
//! Rating messages to the relevant channel take the form "RATING( TRACK)?" (the two components can
//! be separated by any whitespace). The rating can be given by an integer between 0 & 255
//! (inclusive) represented in base ten, or as one-to-five asterisks (i.e. \*{1,5}). In the latter
//! case, the rating will be mapped to 1-255 as per Winamp's
//! [convention](http://forums.winamp.com/showpost.php?p=2903240&postcount=94):
//!
//!   - 224-255 :: 5 stars when READ with windows explorer, writes 255
//!   - 160-223 :: 4 stars when READ with windows explorer, writes 196
//!   - 096-159 :: 3 stars when READ with windows explorer, writes 128
//!   - 032-095 :: 2 stars when READ with windows explorer, writes 64
//!   - 001-031 :: 1 stars when READ with windows explorer, writes 1
//!
//! NB a rating of zero means "not rated".
//!
//! Everything after the first whitepace, if present, is taken to be the track to be rated (i.e.
//! the track may contain whitespace). If omitted, the rating is taken to apply to the current
//! track.

use crate::clients::{Client, PlayerStatus};
use crate::commands::spawn;

use log::debug;
use snafu::{Backtrace, GenerateBacktrace, OptionExt, ResultExt, Snafu};

use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                             Error                                              //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An enumeration of ratings errors
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("{}", cause))]
    Other {
        #[snafu(source(true))]
        cause: Box<dyn std::error::Error>,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
    /// Unable to interpret a string as a rating
    #[snafu(display("Couldn't interpret `{}' as a rating", text))]
    RatingError {
        #[snafu(source(from(std::num::ParseIntError, Box::new)))]
        cause: Box<dyn std::error::Error>,
        text: String,
    },
    #[snafu(display("Can't rate the current track when the player is stopped"))]
    PlayerStopped,
    #[snafu(display("`{}' is not implemented, yet", feature))]
    NotImplemented { feature: String },
    #[snafu(display("Path `{}' cannot be converted to a String", pth.display()))]
    BadPath {
        pth: PathBuf,
        #[snafu(backtrace(true))]
        back: Backtrace,
    },
}

// TODO(sp1ff): re-factor this into one place
macro_rules! error_from {
    ($t:ty) => {
        impl std::convert::From<$t> for Error {
            fn from(err: $t) -> Self {
                Error::Other {
                    cause: Box::new(err),
                    back: Backtrace::generate(),
                }
            }
        }
    };
}

error_from!(crate::clients::Error);
error_from!(crate::commands::Error);
error_from!(std::num::ParseIntError);

pub type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                     RatingRequest message                                      //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// The track to which a rating shall be applied.
#[derive(Debug, PartialEq)]
pub enum RatedTrack {
    Current,
    File(std::path::PathBuf),
    Relative(i8),
}

/// A request from a client to rate a track.
#[derive(Debug)]
pub struct RatingRequest {
    pub rating: u8,
    pub track: RatedTrack,
}

/// Produce a RatingRequest instance from a line of `mpd' output.
impl std::convert::TryFrom<&str> for RatingRequest {
    type Error = Error;

    /// Attempt to produce a RatingRequest instance from a line of `mpd' response to a
    /// "readmessages" command. After the channel line, each subsequent line will be of the form
    /// "message: $MESSAGE"-- this method assumes that the "message: " prefix has been stripped off
    /// (i.e. we're dealing with a single line of text containing only our custom message format).
    ///
    /// For ratings, we expect a message of the form: "RATING (TRACK)?".
    fn try_from(text: &str) -> std::result::Result<Self, Self::Error> {
        // We expect a message of the form: "RATING (TRACK)?"; let us split `text' into those two
        // components for separate processing:
        let text = text.trim();
        let (rating, track) = match text.find(char::is_whitespace) {
            Some(idx) => (&text[..idx], &text[idx + 1..]),
            None => (text, ""),
        };

        // Rating first-- the desired rating can be specified in a few ways...
        let rating = if rating.len() == 0 {
            // an empty string is interpreted as zero:
            0u8
        } else {
            // "*{1,5}" is interpreted as one-five stars, mapped to [0,255] as per Winamp:
            match rating {
                "*" => 1,
                "**" => 64,
                "***" => 128,
                "****" => 196,
                "*****" => 255,
                // failing that, we try just interperting `rating' as an unsigned integer:
                _ => rating.parse::<u8>().context(RatingError {
                    text: String::from(rating),
                })?,
            }
        };

        // Next-- track. This, too, can be given in a few ways:
        let track = if track.len() == 0 {
            // nothing at all just means "current track"
            RatedTrack::Current
        } else {
            // otherwise...
            match text.parse::<i8>() {
                // if we can interpret `track' as an i8, we take it as an offset...
                Ok(i) => RatedTrack::Relative(i),
                // else, we assume it's a path. If it's not, we'll figure that out downstream.
                Err(_) => RatedTrack::File(std::path::PathBuf::from(&track)),
            }
        };

        Ok(RatingRequest {
            rating: rating,
            track: track,
        })
    }
}

#[cfg(test)]
mod rating_request_tests {

    use super::*;

    /// RatingRequest smoke tests
    #[test]
    fn rating_request_smoke() {
        let req = RatingRequest::try_from("*** foo bar splat.mp3").unwrap();
        assert_eq!(req.rating, 128);
        assert_eq!(
            req.track,
            RatedTrack::File(PathBuf::from("foo bar splat.mp3"))
        );
        let req = RatingRequest::try_from("255").unwrap();
        assert_eq!(req.rating, 255);
        assert_eq!(req.track, RatedTrack::Current);
        let _req = RatingRequest::try_from("******").unwrap_err();
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                           Rating Ops                                           //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Retrieve the rating for a track as an unsigned int from zero to 255
pub async fn get_rating(client: &mut Client, sticker: &str, file: &str) -> Result<u8> {
    match client.get_sticker(file, sticker).await? {
        Some(text) => Ok(text.parse::<u8>()?),
        None => Ok(0u8),
    }
}

/// Core routine for setting the rating for a track-- will run the associated command, if present
pub async fn set_rating<I: Iterator<Item = String>>(
    client: &mut Client,
    sticker: &str,
    file: &str,
    rating: u8,
    cmd: &str,
    args: &mut I,
    music_dir: &str,
) -> Result<Option<crate::commands::PinnedCmdFut>> {
    client
        .set_sticker(file, sticker, &format!("{}", rating))
        .await?;

    // TODO(sp1ff): factor out command-related logic?
    if cmd.len() == 0 {
        return Ok(None);
    }
    let mut params = HashMap::<String, String>::new();

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
    params.insert("rating".to_string(), format!("{}", rating));

    Ok(Some(spawn(cmd, args, &params).await?))
}

// TODO(sp1ff): this interface needs to be re-factored
/// Take the command message from our channel, apply it to the ratings for the given song,
/// and optionally run a command on the server (presumably to keep the track's ID3 tags
/// up-to-date).
pub async fn handle_rating<I: Iterator<Item = String>>(
    msg: &str,
    client: &mut Client,
    rating_sticker: &str,
    ratings_cmd: &str,
    ratings_cmd_args: &mut I,
    music_dir: &str,
    play_state: &PlayerStatus,
) -> Result<Option<crate::commands::PinnedCmdFut>> {
    let req = RatingRequest::try_from(msg)?;
    let pathb = match req.track {
        RatedTrack::Current => match play_state {
            PlayerStatus::Stopped => {
                return Err(Error::PlayerStopped {});
            }
            PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => curr.file.clone(),
        },
        RatedTrack::File(p) => p,
        RatedTrack::Relative(_i) => {
            return Err(Error::NotImplemented {
                feature: String::from("Relative track position"),
            });
        }
    };
    let pth: &str = pathb.to_str().context(BadPath { pth: pathb.clone() })?;

    debug!("Setting a rating of {} for `{}'.", req.rating, pth);

    set_rating(
        client,
        rating_sticker,
        pth,
        req.rating,
        ratings_cmd,
        ratings_cmd_args,
        music_dir,
    )
    .await
}
