//! What the dossier shows about one process beyond the table row: its
//! threads, open files, sockets and working directory. Gathered on demand
//! for a single pid, never for the whole list.

/// One thread of the inspected process.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadInfo {
    pub id: u64,
    /// `pthread_setname_np` name; empty when unnamed.
    pub name: String,
    /// User + system CPU time so far, in nanoseconds.
    pub cpu_ns: u64,
    /// Kernel CPU-usage estimate, percent of one core.
    pub cpu_pct: f32,
}

/// One open file descriptor backed by a path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenFile {
    pub fd: i32,
    pub path: String,
}

/// One open socket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Socket {
    pub fd: i32,
    /// "tcp", "udp", "unix" or "other".
    pub kind: &'static str,
    pub local_port: Option<u16>,
    pub remote_port: Option<u16>,
    /// Accepting connections (`SO_ACCEPTCONN`).
    pub listening: bool,
}

/// Everything `inspect(pid)` found. Empty vectors mean "none", not
/// "unreadable": `denied` says the kernel refused, `recorded` that the pid
/// came from a replay or the demo and nothing live was read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inspection {
    pub pid: u32,
    pub threads: Vec<ThreadInfo>,
    pub files: Vec<OpenFile>,
    pub sockets: Vec<Socket>,
    pub cwd: Option<String>,
    /// The kernel refused (another user's process without root).
    pub denied: bool,
    /// The pid belongs to a recording (a replay or the demo), so this Mac's
    /// process with that number, if any, is not the one shown.
    pub recorded: bool,
}

impl Inspection {
    /// Distinct local ports the process listens on, ascending.
    #[must_use]
    pub fn listening_ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self
            .sockets
            .iter()
            .filter(|s| s.listening)
            .filter_map(|s| s.local_port)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_ports_are_sorted_distinct_and_listening_only() {
        let sock = |port, listening| Socket {
            fd: 3,
            kind: "tcp",
            local_port: Some(port),
            remote_port: None,
            listening,
        };
        let ins = Inspection {
            sockets: vec![
                sock(8080, true),
                sock(22, true),
                sock(8080, true),
                sock(5000, false),
            ],
            ..Inspection::default()
        };
        assert_eq!(ins.listening_ports(), [22, 8080]);
    }
}
