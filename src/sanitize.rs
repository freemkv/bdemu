//! bdemu terminal-output sanitiser, shared via `#[path = "sanitize.rs"]`
//! between the cdylib and CLI. Drive profiles are untrusted
//! (GitHub-issue-sourced); `.trim()` alone won't stop ESC sequences.

// Replacement for a control character. A visible placeholder beats deletion:
// deleting would let "PIONE\x08\x08\x08ACME" render as a plausible different
// vendor, whereas `PIONE???ACME` shows the operator the profile is lying.
const REPLACEMENT: char = '?';

/// True for the Unicode bidi/format control characters used in "Trojan Source"
/// attacks (e.g. U+202E RIGHT-TO-LEFT OVERRIDE) to make text render in an order
/// that misleads a reader. `char::is_control` misses these: they are Cf, not Cc.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Make a profile-derived (or otherwise untrusted) string safe to print to a
/// terminal on a single line.
///
/// Replaces every Unicode control character (`char::is_control`: C0/ESC/CR/LF/
/// TAB, DEL, C1) — covering escape introducers and forged newlines — plus the
/// Cf bidi/format controls used in "Trojan Source" attacks (e.g. U+202E).
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || is_bidi_control(c) {
                REPLACEMENT
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_terminal;

    /// Catches the mutation that drops the control-character filter (or narrows
    /// it to only `\n`/`\r`): an ESC-bearing INQUIRY product string from a
    /// stranger's profile must not reach the terminal intact.
    #[test]
    fn escape_sequences_are_neutralised() {
        // The concrete attack: a profile whose product string clears the screen
        // and rewrites the window title.
        let hostile = "BDR\x1b[2J\x1b]0;pwned\x07";
        let clean = sanitize_for_terminal(hostile);
        assert!(!clean.contains('\x1b'), "ESC must not survive: {clean:?}");
        assert!(!clean.contains('\x07'), "BEL must not survive: {clean:?}");
        assert_eq!(clean, "BDR?[2J?]0;pwned?");
    }

    // Catches a filter that only handles ESC: newline injection would let a
    // single profile field forge extra `validate` report lines, and DEL/C1
    // carry the same escape semantics as ESC on many terminals.
    #[test]
    fn newlines_del_and_c1_are_neutralised() {
        assert_eq!(sanitize_for_terminal("a\nb\r\tc"), "a?b??c");
        assert_eq!(sanitize_for_terminal("a\x7fb"), "a?b");
        // U+009B is the C1 CSI — an escape introducer in its own right.
        assert_eq!(sanitize_for_terminal("a\u{9b}31mb"), "a?31mb");
    }

    // Catches a filter that only strips `char::is_control`: U+202E reorders the
    // rendered bytes that follow it without being a Cc control, so a profile
    // field could visually disguise a malicious filename/extension.
    #[test]
    fn trojan_source_bidi_override_is_neutralised() {
        let hostile = "cmd.exe\u{202e}gpj.nrocinu";
        let clean = sanitize_for_terminal(hostile);
        assert!(
            !clean.contains('\u{202e}'),
            "RLO must not survive: {clean:?}"
        );
        assert_eq!(clean, "cmd.exe?gpj.nrocinu");
    }

    /// Catches an over-broad filter: legitimate profile text (spaces, punctuation
    /// and non-ASCII disc titles) must pass through byte-for-byte, or the
    /// sanitiser becomes its own information-loss bug.
    #[test]
    fn ordinary_text_is_untouched() {
        assert_eq!(
            sanitize_for_terminal("PIONEER BD-RW   BDR-S09"),
            "PIONEER BD-RW   BDR-S09"
        );
        assert_eq!(sanitize_for_terminal("Amélie_2001"), "Amélie_2001");
        assert_eq!(sanitize_for_terminal(""), "");
    }
}
