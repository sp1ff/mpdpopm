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

//! Running commands on the server
//!
//! # Introduction
//!
//! [mpdpopm](crate) allows for running arbitrary programs on the server in response to server
//! events or external commands (to keep ID3 tags up-to-date when a song is rated, for
//! instance). This may seem like a vulnerability if your [MPD] server is listening on a socket, but
//! it's not like callers can execute arbitrary code: certain events can trigger commands that you,
//! the [MPD] server owner have configured.
//!
//! [MPD]: https://www.musicpd.org/ "MPD"
//!
//! # The Generalized Command Framework
//!
//! In addition to fixed commands (i.e. commands that are run in response to particular events, like
//! rating a song, or completing a track), [mpdpopm](https://github.com/sp1ff/mpdpopm) provides a
//! generalized command framework by which you can map arbitrary messages sent to the
//! [mpdpopm](crate) channel to code execution on the server side. A generalized command can be
//! described as:
//!
//! 1. the command name: commands shall be named according to the regular expression:
//!    [a-zA-Z][-a-zA-Z0-9]+
//!
//! 2. command parameters: a command may take parameters; the parameters are defined by an array
//!    of zero or more instances of:
//!
//!    - parameter name: parameter names shall match the regex: [a-zA-Z][-a-zA-Z0-9]+
//!
//!    - parameter type: parameters may be a:
//!
//!      - general/string
//!
//!      - song (empty means "current", otherwise song URI, maybe someday index into playlist)
//!
//!    parameters beyond a certain point may be marked "optional"; optional parameters that are
//!    not provided shall be given a default value (the empty string for general parameters, or
//!    the current track for a song parameter)
//!
//! 3. program to run, with arguments; arguments can be:
//!
//!    - string literals (program options, for instance)
//!
//!    - replacement parameters
//!
//!      - %1, %2, %3... will be replaced with the parameter in the corresponding position above
//!
//!      - %full-file will expand to the absolute path to the song named by the track argument, if
//!      present; if this parameter is used, and the track is *not* provided in the command
//!      arguments, it is an error
//!
//!      - %rating, %play-count & %last-played will expand to the value of the corresponding
//!      sticker, if present. If any of these are used, and the track is *not* provided in the
//!      command arguments, it is an error. If the corresponding sticker is not present in the
//!      sticker database, default values of 0, 0, and Jan. 1 1970 (i.e. Unix epoch) will be
//!      provided instead.
//!
//! ## Quoting
//!
//! [MPD](https://www.musicpd.org/) breaks up commands into their constituent tokens on the basis
//! of whitespace:
//!
//! ```text
//! token := [a-zA-Z0-09~!@#$%^&*()-=_+[]{}\|;:<>,./?]+ | "([ \t'a-zA-Z0-09~!@#$%^&*()-=_+[]{}|;:<>,./?]|\"|\\)"
//! ```
//!
//! In other words, a token is any group of ASCII, non-whitespace characters except for ' & " (even
//! \). In order to include space, tab, ' or " the token must be enclosed in double quotes at which
//! point " and \ (but *not* ') must be backslash-escaped. Details can be found
//! [here](https://www.musicpd.org/doc/html/protocol.html#escaping-string-values) and
//! [here](https://github.com/MusicPlayerDaemon/MPD/blob/master/src/util/Tokenizer.cxx#L170).
//!
//! This means that when invoking generalized commands via the MPD messages mechanism, the
//! entire command will need to be quoted *twice* on the client side. An example will help
//! illustrate. Suppose the command is named myfind and we wish to pass an argument which contains
//! both ' characters & spaces:
//!
//! ```text
//! myfind (Artist == 'foo' and rating > '***')
//! ^^^^^^ ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//! cmd    argument
//! ```
//!
//! Since the argument contains spaces _and_ '-s we'll need to enclose it in double quotes (at
//! which point the ' characters become permissible):
//!
//! ```text
//! myfind "(Artist == 'foo' and rating > '***')"
//! ```
//!
//! Since this entire command will itself be a single token in the "sendmessage" command, we
//! need to quote it in its entirety on the client side:
//!
//! ```text
//! "myfind \"(Artist == 'foo' and rating > '***')\""
//! ```
//!
//! (Enclose the entire command in double-quotes, then backslash-escape any " or \ characters
//! therein).
//!
//! The MPD docs suggest using a client library such as
//! [libmpdclient](https://www.musicpd.org/libs/libmpdclient/) to do the quoting, but the
//! implementation is
//! [straightforward](https://github.com/MusicPlayerDaemon/libmpdclient/blob/master/src/quote.c#L37).

use crate::clients::PlayerStatus;

use backtrace::Backtrace;
use futures::future::Future;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                       module Error type                                        //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// An mpdpopm command error
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    BadPath {
        pth: PathBuf,
        back: backtrace::Backtrace,
    },
    DuplicateTrackArgument {
        index: usize,
        back: backtrace::Backtrace,
    },
    MissingParameter {
        index: usize,
        back: backtrace::Backtrace,
    },
    NoCurrentTrack,
    NoTrackToUpdate,
    TrailingPercent {
        template: String,
        back: backtrace::Backtrace,
    },
    UnknownParameter {
        param: String,
        back: backtrace::Backtrace,
    },
}

impl std::fmt::Display for Error {
    #[allow(unreachable_patterns)] // the _ arm is *currently* unreachable
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::BadPath { pth, back: _ } => write!(f, "Bad path: {:?}", pth),
            Error::DuplicateTrackArgument { index, back: _ } => {
                write!(f, "Duplicate track argument at index {}", index)
            }
            Error::MissingParameter { index, back: _ } => {
                write!(f, "Missing actual parameter at index {}", index)
            }
            Error::NoCurrentTrack => write!(f, "No current track"),
            Error::NoTrackToUpdate => write!(f, "No track to update"),
            Error::TrailingPercent { template, back: _ } => {
                write!(f, "Trailing percent in template ``{}''", template)
            }
            Error::UnknownParameter { param, back: _ } => {
                write!(f, "Unknown parameter ``{}''", param)
            }
            _ => write!(f, "Unknown commands error"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Option::None
    }
}

type Result<T> = std::result::Result<T, Error>;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                      replacement strings                                       //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Process a replacement string with replacement parameters of the form "%param" given a lookup
/// table for parameter replacements. Literal `%'-s can be expressed as "%%".
pub fn process_replacements(templ: &str, params: &HashMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut c = templ.chars().peekable();
    loop {
        let a = match c.next() {
            Some(x) => x,
            None => {
                break;
            }
        };
        if a != '%' {
            out.push(a);
        } else {
            let b = c.peek().ok_or_else(|| Error::TrailingPercent {
                template: String::from(templ),
                back: Backtrace::new(),
            })?;
            if *b == '%' {
                c.next();
                out.push('%');
            } else {
                let mut terminal = None;
                let t: String = c
                    .by_ref()
                    .take_while(|x| {
                        if x.is_alphanumeric() || x == &'-' || x == &'_' {
                            true
                        } else {
                            terminal = Some(x.clone());
                            false
                        }
                    })
                    .collect();
                out.push_str(params.get(&t).ok_or_else(|| Error::UnknownParameter {
                    param: String::from(t),
                    back: Backtrace::new(),
                })?);
                match terminal {
                    Some(x) => out.push(x),
                    None => {
                        break;
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod test_replacement_strings {
    #[test]
    fn test_process_replacements() {
        use super::process_replacements;
        use std::collections::HashMap;
        let mut p: HashMap<String, String> = HashMap::new();
        p.insert(String::from("rating"), String::from("255"));
        assert_eq!(
            "rating is 255",
            process_replacements("rating is %rating", &p).unwrap()
        );
        p.insert(String::from("full-path"), String::from("has spaces"));
        assert_eq!(
            "\"has spaces\" has rating 255",
            process_replacements("\"%full-path\" has rating %rating", &p).unwrap()
        );
    }
}

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                            commands                                            //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// A Tokio Command Output future
pub type PinnedCmdFut =
    std::pin::Pin<Box<dyn Future<Output = tokio::io::Result<std::process::Output>>>>;

/// Start process cmd with args I & return; the results will be available asynchronously
///
/// If cmd is not an absolute path, `PATH` will be searched in an OS-defined way.
pub fn spawn<S, I>(cmd: S, args: I, params: &HashMap<String, String>) -> Result<PinnedCmdFut>
where
    I: Iterator<Item = String>,
    S: AsRef<OsStr> + std::fmt::Debug,
{
    // let cmd = process_replacements(cmd, &params)?;

    let args: std::result::Result<Vec<_>, _> =
        args.map(|x| process_replacements(&x, &params)).collect();

    match args {
        Ok(a) => {
            info!("Running command `{:#?}' with args {:#?}", &cmd, &a);
            Ok(Box::pin(Command::new(&cmd).args(a).output()))
        }
        Err(err) => Err(Error::from(err)),
    }
}

use pin_project::pin_project;
use std::pin::Pin;

/// A Tokio Command Output future, tagged with additional information
///
/// I had originally written commands in terms of Future<tokio::io::Result<std::process::Output>>;
/// i.e. futures yielding the results of to-be-completed commands. Then I realized that _some_
/// commands require a database update *after* they've completed; in other words, I need a way
/// to reap that information at command completion time.
#[pin_project]
pub struct TaggedCommandFuture {
    #[pin]
    /// Future yielding our command results
    fut: PinnedCmdFut,
    /// This is a little "tag" that comes along with the [`Command`] result; the command initiator
    /// can annotate the command to indicate whether or not an mpd database update is required upon
    /// completion. None means no database required, Some(uri) means `uri` needs to be updated
    upd: Option<String>,
}

impl TaggedCommandFuture {
    pub fn new(fut: PinnedCmdFut, upd: Option<String>) -> TaggedCommandFuture {
        TaggedCommandFuture { fut: fut, upd: upd }
    }
    pub fn pin(fut: PinnedCmdFut, upd: Option<String>) -> std::pin::Pin<Box<TaggedCommandFuture>> {
        Box::pin(TaggedCommandFuture::new(fut, upd))
    }
}

/// [`TaggedCommandFuture`] implements [`Future`]; this is what it yields
pub struct TaggedCommandOutput {
    pub out: tokio::io::Result<std::process::Output>,
    pub upd: Option<String>,
}

/// [`Future`] implementation that delegates to the tokio Command-- when it completes it will
/// return the Command result along with whether a database update is required, and the
/// salient song if so.
impl std::future::Future for TaggedCommandFuture {
    type Output = TaggedCommandOutput;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Self::Output> {
        // This was surprisingly difficult to implement, and I wound up out-sourcing it to the
        // `pin_project` crate.
        //
        // The problem is that `poll' is only defined for Pin<&mut Thing>, where `Thing' is the type
        // on which `Future` is implemented. So in this case, `self' is Pin<&mut
        // TaggedCommandFuture>. In order to call `poll' on self.fut, I need that to be
        // Pin<&mut Box<dyn Future>>, but it is Pin<Box<dyn Future>>-- I spent an afternoon
        // trying to figure out how to get the "mut" inside the Pin.
        //
        // I found a solution here:
        // <https://stackoverflow.com/questions/57369123/no-method-named-poll-found-for-a-type-that-implements-future>
        //
        // "You need to project the pinning from your type to the field."
        //
        // TBH, I'm not sure this is even safe (pretty sure it uses `unsafe' code):
        let this = self.project();
        // Move the "mut" inside the Pin...
        let fut: Pin<&mut PinnedCmdFut> = this.fut;
        // I've already moved `self' into `this', so grab the `upd' field, as well.
        let upd: &mut Option<String> = this.upd;
        // NOW I can delegate to `self.fut'...
        match fut.poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            // and pass along the update information, if needed.
            std::task::Poll::Ready(out) => std::task::Poll::Ready(TaggedCommandOutput {
                out: out,
                upd: upd.clone(),
            }),
        }
    }
}

/// Convenience alias for a boxed-then-pinned TaggedCommandFuture
pub type PinnedTaggedCmdFuture = std::pin::Pin<std::boxed::Box<TaggedCommandFuture>>;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                      generalized commands                                      //
////////////////////////////////////////////////////////////////////////////////////////////////////

/// Describes the formal parameter type
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum FormalParameter {
    Literal,
    Track,
}

/// Describes the sort of database update that should take place on command completion
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Update {
    NoUpdate,
    TrackOnly,
    FullDatabase,
}

/// A general command that will be run on the server, invoked by an MPD message
pub struct GeneralizedCommand {
    /// Formal parameters
    formal_parameters: Vec<FormalParameter>,
    /// Formal parameters assume their default after this many places
    default_after: usize,
    /// MPD music directory -- used to form absolute paths
    music_dir: PathBuf,
    /// The command to be run; if not absolute, the `PATH` will be searched in an system-
    /// dependent way
    cmd: PathBuf,
    /// Command arguments; may include replacement parameters (like "%full-file")
    args: Vec<String>,
    /// The sort of MPD music database update that needs to take place when this command finishes
    update: Update,
}

impl GeneralizedCommand {
    pub fn new<I1, I2>(
        formal_params: I1,
        default_after: usize,
        music_dir: &str,
        cmd: &PathBuf,
        args: I2,
        update: Update,
    ) -> GeneralizedCommand
    where
        I1: Iterator<Item = FormalParameter>,
        I2: Iterator<Item = String>,
    {
        GeneralizedCommand {
            formal_parameters: formal_params.collect(),
            default_after: default_after,
            music_dir: PathBuf::from(music_dir),
            cmd: cmd.clone(),
            args: args.collect(),
            update: update,
        }
    }

    /// Execute a general command
    ///
    /// `tokens` shall be an iterator over the message tokens, beginning with the first token
    /// *after* the command name
    pub fn execute<'a, I>(&self, tokens: I, state: &PlayerStatus) -> Result<PinnedTaggedCmdFuture>
    where
        I: Iterator<Item = &'a str>,
    {
        // Prepare replacement parameters
        let mut params = HashMap::<String, String>::new();
        // We always provide %current-file (the full path to the currently playing track), if
        // possible.
        let current_file = match state {
            PlayerStatus::Stopped => None,
            PlayerStatus::Play(curr) | PlayerStatus::Pause(curr) => {
                let mut cfp = self.music_dir.clone();
                cfp.push(&curr.file);
                let cfs = cfp
                    .to_str()
                    .ok_or_else(|| Error::BadPath {
                        pth: cfp.clone(),
                        back: Backtrace::new(),
                    })?
                    .to_string();
                params.insert("current-file".to_string(), cfs.clone());
                debug!("current-file is: {}", cfs);
                Some(cfs)
            }
        };
        // Now walk our formal parameters...
        let mut i: usize = 1;
        let mut saw_track = false;
        let mut full_file: Option<String> = None;
        let mut act_params = tokens.into_iter();
        for form_param in &self.formal_parameters {
            let act_param = act_params.next();
            match (form_param, act_param) {
                (FormalParameter::Literal, Some(token)) => {
                    // Simple case-- replacement parameter %i will be "token"
                    params.insert(format!("{}", i), token.into());
                }
                (FormalParameter::Literal, None) => {
                    // Slightly more complicated, replacement parameter %i will be "", but only
                    // if this formal parameter is allowed to be defaulted.
                    if i < self.default_after {
                        return Err(Error::MissingParameter {
                            index: i,
                            back: Backtrace::new(),
                        });
                    }
                    debug!("%{} is: nil", i);
                    params.insert(format!("{}", i), String::from(""));
                }
                (FormalParameter::Track, Some(token)) => {
                    if saw_track {
                        return Err(Error::DuplicateTrackArgument {
                            index: i,
                            back: Backtrace::new(),
                        });
                    }
                    let mut ffp = self.music_dir.clone();
                    ffp.push(PathBuf::from(token));
                    let ffs = ffp.to_str().ok_or_else(|| Error::BadPath {
                        pth: ffp.clone(),
                        back: Backtrace::new(),
                    })?;
                    params.insert(format!("{}", i), ffs.to_string());
                    params.insert("full-file".to_string(), ffs.to_string());
                    full_file = Some(ffs.to_string());
                    saw_track = true;
                }
                (FormalParameter::Track, None) => {
                    if i < self.default_after {
                        return Err(Error::MissingParameter {
                            index: i,
                            back: Backtrace::new(),
                        });
                    }
                    if saw_track {
                        return Err(Error::DuplicateTrackArgument {
                            index: i,
                            back: Backtrace::new(),
                        });
                    }
                    match &current_file {
                        Some(cf) => {
                            full_file = Some(cf.clone());
                            params.insert(format!("{}", i), cf.clone());
                            params.insert("full-file".to_string(), cf.to_string());
                        }
                        None => {
                            return Err(Error::NoCurrentTrack);
                        }
                    }
                    saw_track = true;
                }
            }

            i += 1;
        }

        // Take the PinnedCmdFuture we get from `spawn' & combine it with the update type for our
        // caller...
        Ok(TaggedCommandFuture::pin(
            // spawn the process-- returns a PinnedCmdFuture
            spawn(&self.cmd, self.args.iter().cloned(), &params)?,
            // map self.update to an Option<String>
            match self.update {
                Update::NoUpdate => None,
                Update::TrackOnly => {
                    // At this point:
                    //     - current_file is the absolute path to the current track, if any (None else)
                    //     - full_file is the absolute path given in the command arguments, which may be the
                    //       same as current_file, or may be different; it may also be None
                    // In terms of DB updates, we give preference to full_file, then current_file
                    match full_file {
                        Some(x) => Some(format!("song {}", x)),
                        None => match current_file {
                            Some(x) => Some(format!("song {}", x)),
                            None => return Err(Error::NoTrackToUpdate),
                        },
                    }
                }
                Update::FullDatabase => Some(String::from("")),
            },
        ))
    }
}
