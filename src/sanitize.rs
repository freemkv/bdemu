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
#[path = "sanitize_tests.rs"]
mod tests;
