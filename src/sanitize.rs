// bdemu — Terminal-output sanitiser, shared via `#[path = "sanitize.rs"]`
// between the cdylib and CLI. Drive profiles are untrusted (GitHub-issue-
// sourced); `.trim()` alone won't stop ESC sequences hiding a MISSING line.

/// Replacement for a control character. A visible, non-ambiguous placeholder is
/// better than deletion: deleting would let "PIONE\x08\x08\x08ACME" render as a
/// plausible-looking different vendor, whereas `PIONE???ACME` shows the operator
/// that the profile is lying to them.
const REPLACEMENT: char = '?';

/// Make a profile-derived (or otherwise untrusted) string safe to print to a
/// terminal on a single line.
///
/// Every Unicode control character — C0 (including ESC U+001B, CR, LF, TAB), DEL
/// U+007F and C1 U+0080–U+009F, i.e. exactly `char::is_control` — is replaced by
/// `?`. That covers the escape-sequence introducers (ESC, and the C1 CSI/OSC
/// forms) as well as the newline injection that would let one profile field forge
/// additional report lines.
pub fn sanitize_for_terminal(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { REPLACEMENT } else { c })
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

    /// Catches a filter that only handles ESC: newline injection would let a
    /// single profile field forge extra `validate` report lines (e.g. a fake
    /// "✓ sectors.bin" under a disc that has none), and DEL/C1 carry the same
    /// escape semantics as ESC on many terminals.
    #[test]
    fn newlines_del_and_c1_are_neutralised() {
        assert_eq!(sanitize_for_terminal("a\nb\r\tc"), "a?b??c");
        assert_eq!(sanitize_for_terminal("a\x7fb"), "a?b");
        // U+009B is the C1 CSI — an escape introducer in its own right.
        assert_eq!(sanitize_for_terminal("a\u{9b}31mb"), "a?31mb");
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
