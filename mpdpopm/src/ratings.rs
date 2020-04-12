//! Logic for rating MPD tracks.

//! Cf. the [MPD protocol](http://www.musicpd.org/doc/protocol/).

use crate::client::*;
use crate::{process_replacements, PinnedCmdFut, ReplacementStringError};

use futures::stream::FuturesUnordered;
use log::{debug, info};

use std::collections::HashMap;
use std::convert::TryFrom;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////

/// The track to which a rating shall be applied.
#[derive(Debug)]
enum RatedTrack {
    Current,
    File(std::path::PathBuf),
    Relative(i8),
}

/// A request from a client to rate a track.
#[derive(Debug)]
struct RatingRequest {
    pub rating: u8,
    pub track: RatedTrack,
}

#[derive(Clone, Debug)]
pub struct RatingError {
    msg: String,
}

impl RatingError {
    pub fn new(msg: &str) -> RatingError {
        RatingError {
            msg: msg.to_string(),
        }
    }
}

impl fmt::Display for RatingError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl Error for RatingError {
    fn description(&self) -> &str {
        &self.msg
    }
}

impl From<MpdClientError> for RatingError {
    fn from(err: MpdClientError) -> Self {
        RatingError {
            msg: format!("mpd client error: {:#?}", err),
        }
    }
}

impl From<ReplacementStringError> for RatingError {
    fn from(err: ReplacementStringError) -> Self {
        RatingError {
            msg: format!("replacement string error: {:#?}", err),
        }
    }
}

/// Produce a RatingRequest instance from a line of `mpd' output.
impl std::convert::TryFrom<&str> for RatingRequest {
    type Error = RatingError;

    /// Attempt to produce a RatingRequest instance from a line of `mpd' response to a
    /// "readmessages" command. After the channel line, each subsequent line will be of the form
    /// "message: $MESSAGE"-- this method assumes that the "message: " prefix has been stripped off
    /// (i.e. we're dealing with a single line of text containing only our custom message format).
    ///
    /// For ratings, we expect a message of the form: "RATING (TRACK)?".
    fn try_from(text: &str) -> Result<Self, Self::Error> {
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
                // failing that, we try just interperting `rating' as an unsigned inteter:
                _ => match rating.parse::<u8>() {
                    Ok(u) => u,
                    Err(err) => {
                        return Err(RatingError::new(&format!(
                            "Unable to format '{}' as a rating: {}",
                            rating, err
                        )))
                    }
                },
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

pub async fn check_messages(
    client: &mut Client,
    idle_client: &mut IdleClient,
    ratings_chan: &str,
    rating_sticker: &str,
    ratings_cmd: &str,
    ratings_cmd_args: &Vec<String>,
    music_dir: &PathBuf,
    status: &ServerStatus,
    cmds: &mut FuturesUnordered<PinnedCmdFut>,
) -> Result<(), RatingError> {
    // If a ratings message is sent, we'll get: "changed: message\nOK\n", to which we respond
    // "readmessages", which should give us something like:
    // "readmessages\nchannel: ratings\nmessage: 255\nOK\n". We remain subscribed, but we need
    // to send a new idle message.
    let lines = idle_client.get_messages().await?;

    let prefix = format!("channel: {}", ratings_chan);
    let mut ratings = Vec::<RatingRequest>::new();
    let mut in_channel = false;
    for line in lines {
        if !in_channel {
            if line.starts_with(&prefix) {
                in_channel = true;
            }
        } else {
            if !line.starts_with("message: ") {
                in_channel = false;
            } else {
                ratings.push(RatingRequest::try_from(&line[9..])?);
                debug!("Found RatingRequest {:#?}.", ratings.last().unwrap());
            }
        }
    }

    for r in ratings {
        let mut params = HashMap::<String, String>::new();

        let pth = match r.track {
            RatedTrack::Current => status.file.clone(),
            RatedTrack::File(p) => p,
            RatedTrack::Relative(_i) => {
                return Err(RatingError::new(
                    "Relative track position not supported, yet.",
                ));
            }
        };
        let mut path = PathBuf::from(music_dir);
        let pth = match pth.to_str() {
            Some(s) => s.to_string(),
            None => {
                return Err(RatingError::new(&format!(
                    "Couldn't represent {:#?} as UTF-8.",
                    path
                )));
            }
        };

        client
            .set_sticker(&pth, &rating_sticker, &format!("{}", r.rating))
            .await?;

        path.push(pth);

        params.insert(
            "full-file".to_string(),
            match path.to_str() {
                Some(s) => s.to_string(),
                None => {
                    return Err(RatingError::new(&format!(
                        "Couldn't represent {:#?} as UTF-8.",
                        path
                    )));
                }
            },
        );
        params.insert("rating".to_string(), format!("{}", r.rating));

        let cmd = process_replacements(&ratings_cmd, &params)?;
        let args: Result<Vec<_>, _> = ratings_cmd_args
            .iter()
            .map(|x| process_replacements(x, &params))
            .collect();

        // TODO(sp1ff): Not sure this will work! May have to delay editing tags until mpd is done
        // playing the file?
        match args {
            Ok(a) => {
                info!("Running 'rating' command: {} with args {:#?}", cmd, a);
                cmds.push(Box::pin(
                    tokio::process::Command::new(&cmd).args(a).output(),
                ));
            }
            Err(err) => {
                return Err(RatingError::new(&format!(
                    "rating error with args: {}",
                    err
                )));
            }
        }
    }

    Ok(())
}
