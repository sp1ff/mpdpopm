//! Logic for rating MPD tracks.
//!
//! # Introduction
//!
//! This module contains types implementing a basic rating functionality for `mpd'.

use crate::client::Client;
use crate::{process_replacements, PinnedCmdFut, ReplacementStringError};

use log::{debug, info};

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fmt;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                             Error                                              //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An enumeration of ratings errors
#[derive(Debug)]
pub enum Cause {
    /// An error occurred in some sub-system (network I/O, or string formatting, e.g.)
    SubModule,
    /// Unable to interpret a string as a rating
    RatingError(String),
    NotImplemented(String),
    BadPath(std::path::PathBuf),
    ReplacementString,
}

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
            Cause::SubModule => write!(f, "ratings error: {:#?}", self.source),
            Cause::RatingError(x) => write!(f, "couldn't interpret '{}' as a rating", x),
            Cause::NotImplemented(x) => write!(f, "{} is not implemented", x),
            Cause::BadPath(x) => write!(f, "bad path '{:#?}'", x),
            Cause::ReplacementString => write!(f, "Unknown replacement string"),
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

// There *must* be a way to do this generically, but my naive attempts clash with the core
// implementation of From<T> for T.

impl From<crate::client::Error> for Error {
    fn from(err: crate::client::Error) -> Self {
        Error {
            cause: Cause::SubModule,
            source: Some(Box::new(err)),
        }
    }
}

impl From<ReplacementStringError> for Error {
    fn from(err: ReplacementStringError) -> Self {
        Error {
            cause: Cause::ReplacementString,
            source: Some(Box::new(err)),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

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
                // failing that, we try just interperting `rating' as an unsigned inteter:
                _ => match rating.parse::<u8>() {
                    Ok(u) => u,
                    Err(_) => {
                        return Err(Error::new(Cause::RatingError(String::from(rating))));
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

pub async fn do_rating(
    msg: &str,
    client: &mut Client,
    rating_sticker: &str,
    ratings_cmd: &str,
    ratings_cmd_args: &Vec<String>,
    music_dir: &PathBuf,
    file: &std::path::PathBuf,
) -> Result<Option<PinnedCmdFut>> {
    let req = RatingRequest::try_from(msg)?;

    let mut params = HashMap::<String, String>::new();

    let pth = match req.track {
        RatedTrack::Current => file.clone(),
        RatedTrack::File(p) => p,
        RatedTrack::Relative(_i) => {
            return Err(Error::new(Cause::NotImplemented(String::from(
                "Relative track position not supported, yet.",
            ))));
        }
    };

    let mut path = PathBuf::from(music_dir);
    let pth = match pth.to_str() {
        Some(s) => s.to_string(),
        None => {
            return Err(Error::new(Cause::BadPath(path)));
        }
    };

    debug!("Setting a rating of {}.", req.rating);
    client
        .set_sticker(&pth, &rating_sticker, &format!("{}", req.rating))
        .await?;

    if ratings_cmd.len() == 0 {
        return Ok(None);
    }

    path.push(pth);

    params.insert(
        "full-file".to_string(),
        match path.to_str() {
            Some(s) => s.to_string(),
            None => {
                return Err(Error::new(Cause::BadPath(path)));
            }
        },
    );
    params.insert("rating".to_string(), format!("{}", req.rating));

    let cmd = process_replacements(&ratings_cmd, &params)?;
    // TODO(sp1ff): understand how to get this to work:
    // let args: Result<Vec<_>> = ratings_cmd_args
    let args: std::result::Result<Vec<_>, _> = ratings_cmd_args
        .iter()
        .map(|x| process_replacements(x, &params))
        .collect();

    match args {
        Ok(a) => {
            info!("Running 'rating' command: {} with args {:#?}", cmd, a);
            Ok(Some(Box::pin(
                tokio::process::Command::new(&cmd).args(a).output(),
            )))
        }
        Err(err) => Err(Error::from(err)),
    }
}
