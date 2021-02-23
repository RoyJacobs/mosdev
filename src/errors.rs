use crossbeam_channel::SendError;
use itertools::Itertools;
use lsp_server::{Message, ProtocolError};

use crate::core::parser::OwnedLocation;

pub type MosResult<T> = Result<T, MosError>;

#[allow(dead_code)]
#[derive(thiserror::Error, Debug)]
pub enum MosError {
    BuildError(String),
    Clap(#[from] clap::Error),
    Codegen {
        location: Option<OwnedLocation>,
        message: String,
    },
    Io(#[from] std::io::Error),
    Parser {
        location: Option<OwnedLocation>,
        length: usize,
        message: String,
    },
    Multiple(Vec<MosError>),
    Toml(#[from] toml::de::Error),
    Protocol(#[from] ProtocolError),
    Crossbeam(#[from] SendError<Message>),
    Unknown,
}

impl PartialEq for MosError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                MosError::Parser {
                    location: lloc,
                    length: llen,
                    message: lmsg,
                },
                MosError::Parser {
                    location: rloc,
                    length: rlen,
                    message: rmsg,
                },
            ) => lloc == rloc && llen == rlen && lmsg == rmsg,
            (
                MosError::Codegen {
                    location: lloc,
                    message: lmsg,
                },
                MosError::Codegen {
                    location: rloc,
                    message: rmsg,
                },
            ) => lloc == rloc && lmsg == rmsg,
            (MosError::Multiple(lhs), MosError::Multiple(rhs)) => lhs == rhs,
            (MosError::BuildError(lhs), MosError::BuildError(rhs)) => lhs == rhs,
            _ => false,
        }
    }
}

impl<T: Into<MosError>> From<Vec<T>> for MosError {
    fn from(errors: Vec<T>) -> Self {
        Self::Multiple(errors.into_iter().map(|e| e.into()).collect())
    }
}

impl std::fmt::Display for MosError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.format(false))
    }
}

impl MosError {
    pub fn format(&self, use_color: bool) -> String {
        use ansi_term::Colour::Red;

        match self {
            MosError::Parser {
                location,
                length: _,
                message,
            }
            | MosError::Codegen { location, message } => {
                let location = match location {
                    Some(location) => {
                        format!(
                            "{}:{}:{}: ",
                            location.path.to_string_lossy(),
                            location.line,
                            location.column
                        )
                    }
                    None => "".to_string(),
                };
                let err = if use_color {
                    Red.paint("error:")
                } else {
                    "error:".into()
                };
                format!("{}{} {}", location, err, message)
            }
            MosError::Io(err) => format!("{}", err),
            MosError::Clap(err) => format!("{}", err),
            MosError::Toml(err) => format!("{}", err),
            MosError::Protocol(err) => format!("{}", err),
            MosError::Crossbeam(err) => format!("{}", err),
            MosError::BuildError(message) => {
                let err = if use_color {
                    Red.paint("error:")
                } else {
                    "error:".into()
                };
                format!("{} {}", err, message)
            }
            MosError::Unknown => "unknown error".to_string(),
            MosError::Multiple(errors) => errors
                .iter()
                .map(|e| e.format(use_color))
                .collect_vec()
                .join("\n"),
        }
    }
}
