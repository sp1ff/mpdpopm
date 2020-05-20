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

//! commands -- running commands on the server
//!
//! # Introduction
//!
//! [`mpdpopm`] allows for running arbitrary programs on the server in respone to server events
//! or external commands (to keep ID3 tags up-to-date when a song is rated, for instance). This may
//! seem like a vulnerability if your [`mpd`] server is listening on a socket, but it's not like
//! callers can execute arbitrary code: certain events can trigger commands that you, the [`mpd`]
//! server owner have configured.

use futures::future::Future;
use log::info;
use snafu::{OptionExt, Snafu};
use tokio::process::Command;

use std::collections::HashMap;

////////////////////////////////////////////////////////////////////////////////////////////////////
//                                       module Error type                                        //
////////////////////////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display(
        "The template string `{}' has a trailing '%' character, which is illegal.",
        template
    ))]
    TrailingPercent { template: String },
    #[snafu(display("Unknown replacement parameter `{}'", param))]
    UnknownParameter { param: String },
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
            let b = c.peek().context(TrailingPercent {
                template: String::from(templ),
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
                out.push_str(params.get(&t).context(UnknownParameter {
                    param: String::from(t),
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

pub type PinnedCmdFut =
    std::pin::Pin<Box<dyn Future<Output = tokio::io::Result<std::process::Output>>>>;

pub async fn spawn<I: Iterator<Item = String>>(
    cmd: &str,
    args: I,
    params: &HashMap<String, String>,
) -> Result<PinnedCmdFut> {
    let cmd = process_replacements(&cmd, &params)?;

    let args: std::result::Result<Vec<_>, _> =
        args.map(|x| process_replacements(&x, &params)).collect();

    match args {
        Ok(a) => {
            info!("Running command `{}' with args {:#?}", &cmd, &a);
            Ok(Box::pin(Command::new(&cmd).args(a).output()))
        }
        Err(err) => Err(Error::from(err)),
    }
}
