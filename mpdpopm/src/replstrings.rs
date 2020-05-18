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

//! replstrings -- utilities for strings with replacement parameters

use snafu::{OptionExt, Snafu};

use std::{collections::HashMap, string::String};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display(
        "The template string `{}' has a trailing `%' character, which is illegal", template))]
    TrailingPercent {
        template: String
    },
    #[snafu(display("Unknown replacement parameter `{}'", param))]
    UnknownParameter {
        param: String
    },
}

/// Process a replacement string with replacement parameters of the form "%param" given a lookup
/// table for parameter replacements. Literal `%'-s can be expressed as "%%".
pub fn process_replacements(templ: &str, params: &HashMap<String, String>) -> Result<String, Error> {
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
            let b = c.peek().context(TrailingPercent{ template: String::from(templ) })?;
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
                out.push_str(params.get(&t).context(UnknownParameter{ param: String::from(t) })?);
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
mod tests {
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
