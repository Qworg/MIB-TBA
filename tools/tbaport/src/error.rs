//! Errors for the converter.
//!
//! The tool is deterministic and fails loudly: a malformed source record is a
//! `Parse` error naming the file and line, never a silent skip. The only
//! "drops" the tool performs are deliberate and recorded in the drop report.

use std::fmt;
use std::path::PathBuf;

/// A conversion failure.
#[derive(Debug)]
pub enum ConvertError {
    /// Reading a source or output file failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A source record did not match the grammar in `src/db.c`.
    Parse {
        path: PathBuf,
        line: usize,
        message: String,
    },
    /// A `Dice` expression from a source record could not be parsed.
    Dice { context: String, value: String },
    /// A command-line argument was missing or malformed.
    Usage(String),
    /// YAML or JSON (de)serialisation failed.
    Serde(String),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::Io { path, source } => {
                write!(f, "io error on {}: {source}", path.display())
            }
            ConvertError::Parse {
                path,
                line,
                message,
            } => write!(
                f,
                "parse error in {} line {line}: {message}",
                path.display()
            ),
            ConvertError::Dice { context, value } => {
                write!(f, "invalid dice expression `{value}` in {context}")
            }
            ConvertError::Usage(message) => write!(f, "usage: {message}"),
            ConvertError::Serde(message) => write!(f, "serialisation error: {message}"),
        }
    }
}

impl std::error::Error for ConvertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConvertError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<serde_yaml::Error> for ConvertError {
    fn from(value: serde_yaml::Error) -> Self {
        ConvertError::Serde(value.to_string())
    }
}

impl From<serde_json::Error> for ConvertError {
    fn from(value: serde_json::Error) -> Self {
        ConvertError::Serde(value.to_string())
    }
}

/// Shorthand used throughout the converter.
pub type Result<T> = std::result::Result<T, ConvertError>;
