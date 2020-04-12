pub mod client;
pub mod ratings;
pub mod vars;

use std::fmt;
use std::{collections::HashMap, string::String};

pub type PinnedCmdFut = std::pin::Pin<
    Box<dyn futures::future::Future<Output = tokio::io::Result<std::process::Output>>>,
>;

#[derive(Debug, Clone)]
pub struct ReplacementStringError {
    param_name: String,
}

impl ReplacementStringError {
    fn new(param_name: &str) -> ReplacementStringError {
        ReplacementStringError {
            param_name: param_name.to_string(),
        }
    }
}

impl fmt::Display for ReplacementStringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.param_name)
    }
}

pub fn process_replacements(
    templ: &str,
    params: &HashMap<String, String>,
) -> Result<String, ReplacementStringError> {
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
            let b = match c.peek() {
                Some(x) => x,
                None => {
                    return Err(ReplacementStringError::new("trailing %"));
                }
            };
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
                match params.get(&t) {
                    Some(x) => out.push_str(&x),
                    None => {
                        return Err(ReplacementStringError::new(&format!("missing param {}", t)));
                    }
                }
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
        use crate::process_replacements;
        use std::collections::HashMap;
        let mut p: HashMap<String, String> = HashMap::new();
        // assert_eq!("", process_replacements("", &p).unwrap());
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
