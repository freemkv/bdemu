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
