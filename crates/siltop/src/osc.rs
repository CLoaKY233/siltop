//! OSC 52 clipboard writes: the terminal (Ghostty, iTerm2, kitty, tmux
//! with `set-clipboard on`) puts the text on the system clipboard, over
//! ssh too, without a pasteboard dependency.

use std::io::Write;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Terminals cap OSC 52 payloads; stay well under the common 100 kB limit.
const MAX_BYTES: usize = 64 * 1024;

/// Standard padded base64.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                let idx = ((n >> shift) & 0x3f) as usize;
                out.push(char::from(ALPHABET.get(idx).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The escape sequence that copies `text`, or `None` when it is too long
/// for terminals to accept.
#[must_use]
pub fn sequence(text: &str) -> Option<String> {
    (text.len() <= MAX_BYTES).then(|| format!("\x1b]52;c;{}\x07", base64(text.as_bytes())))
}

/// Writes a sequence from [`sequence`] to the terminal.
pub fn emit(out: &mut impl Write, seq: &str) -> std::io::Result<()> {
    out.write_all(seq.as_bytes())?;
    out.flush()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in cases {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
        }
    }

    #[test]
    fn sequence_wraps_the_payload_in_osc52() {
        assert_eq!(sequence("412").as_deref(), Some("\x1b]52;c;NDEy\x07"));
    }

    #[test]
    fn emit_writes_the_sequence_and_oversize_is_refused() {
        let mut buf = Vec::new();
        emit(&mut buf, &sequence("pid").unwrap()).unwrap();
        assert_eq!(buf, b"\x1b]52;c;cGlk\x07");
        assert_eq!(sequence(&"x".repeat(MAX_BYTES + 1)), None);
        assert!(sequence(&"x".repeat(MAX_BYTES)).is_some());
    }

    #[test]
    fn utf8_command_lines_survive() {
        let seq = sequence("open ~/Café").unwrap();
        assert!(seq.starts_with("\x1b]52;c;") && seq.ends_with('\x07'));
    }
}
