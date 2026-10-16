//! Text that comes from outside the program (process names, argv, volume
//! names, adapter names, replayed traces) is untrusted: a control character
//! in it reaches the terminal as an escape sequence, and a bidi override
//! reorders what the user reads. Everything shown or printed is made
//! [`printable`] first.

use std::borrow::Cow;

/// Characters that must never be written to a terminal as data: C0 and C1
/// controls and DEL (escape sequences, bells, cursor moves), plus the bidi
/// embedding, override and isolate marks that can disguise a name.
#[must_use]
pub const fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// `text` with every unsafe character replaced by U+FFFD, which is one
/// column wide and still tells the reader something odd was there.
/// Borrowed when there is nothing to replace.
#[must_use]
pub fn printable(text: &str) -> Cow<'_, str> {
    if text.chars().any(is_unsafe) {
        Cow::Owned(
            text.chars()
                .map(|c| if is_unsafe(c) { '\u{FFFD}' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// [`printable`], taking ownership: no copy when `text` is already clean.
#[must_use]
pub fn into_printable(text: String) -> String {
    match printable(&text) {
        Cow::Borrowed(_) => text,
        Cow::Owned(clean) => clean,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_borrowed_untouched() {
        for text in [
            "",
            "WindowServer",
            "Brave Browser Helper (Renderer)",
            "日本語 ✓ émoji 🚀",
        ] {
            assert!(
                matches!(printable(text), Cow::Borrowed(t) if t == text),
                "{text}"
            );
        }
    }

    #[test]
    fn escape_sequences_in_a_process_name_are_defused() {
        // The name a reviewer's probe process used: OSC 2 sets the window title.
        let name = "x\u{1b}]2;RVBC\u{7}y";
        assert_eq!(printable(name), "x\u{FFFD}]2;RVBC\u{FFFD}y");
        for evil in [
            "\u{1b}[2J",             // CSI clear screen
            "\u{1b}]52;c;aGk=\u{7}", // OSC 52 clipboard write
            "a\u{9b}31mb",           // C1 CSI
            "\u{7f}del",
            "tab\there",
            "new\nline\rreturn",
            "\u{0}nul",
        ] {
            let out = printable(evil);
            assert!(!out.chars().any(is_unsafe), "{evil:?} -> {out:?}");
            assert_eq!(
                out.chars().count(),
                evil.chars().count(),
                "one for one: {evil:?}"
            );
        }
    }

    #[test]
    fn bidi_overrides_cannot_disguise_a_name() {
        // "evil\u{202E}txt.exe" would render as "evilexe.txt".
        let out = printable("evil\u{202E}txt.exe");
        assert_eq!(out, "evil\u{FFFD}txt.exe");
        for c in [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{2066}', '\u{2069}',
        ] {
            assert!(is_unsafe(c), "{c:?}");
        }
    }

    #[test]
    fn every_control_character_is_unsafe_and_ordinary_text_is_not() {
        for c in (0u32..0x20)
            .chain([0x7f])
            .chain(0x80..0xa0)
            .filter_map(char::from_u32)
        {
            assert!(is_unsafe(c), "{:#x}", c as u32);
        }
        for c in [
            'a', 'Z', '0', ' ', '(', '·', '°', '↑', '⣿', '█', '╭', 'é', '日', '🚀', '\u{200D}',
        ] {
            assert!(!is_unsafe(c), "{c:?}");
        }
    }

    #[test]
    fn into_printable_keeps_the_allocation_when_clean() {
        let text = String::from("zed");
        let ptr = text.as_ptr();
        let out = into_printable(text);
        assert_eq!(out.as_ptr(), ptr);
        assert_eq!(into_printable(String::from("a\u{1b}b")), "a\u{FFFD}b");
    }
}
