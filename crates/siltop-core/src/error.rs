use std::fmt;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("system: {0}")]
    System(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("terminal: {0}")]
    Terminal(String),
    #[error("process {pid}: {message}")]
    Process { pid: u32, message: String },
}

impl Error {
    pub fn system(msg: impl Into<String>) -> Self {
        Self::System(msg.into())
    }

    pub fn terminal(msg: impl Into<String>) -> Self {
        Self::Terminal(msg.into())
    }

    pub fn process(pid: u32, message: impl Into<String>) -> Self {
        Self::Process {
            pid,
            message: message.into(),
        }
    }
}

impl From<fmt::Error> for Error {
    fn from(value: fmt::Error) -> Self {
        Self::system(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_display() {
        let err = Error::system("host_statistics64");
        assert_eq!(err.to_string(), "system: host_statistics64");
    }

    #[test]
    fn process_display() {
        let err = Error::process(12, "permission denied");
        assert_eq!(err.to_string(), "process 12: permission denied");
    }
}
