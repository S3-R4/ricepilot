//! Paths in the shell commands ricepilot prints for a person to paste.
//!
//! `doctor`'s fixes, the way back from an adopt, `gc`'s note on a kept adopt
//! entry and the `sh …/rescue.sh` every switch and `--relogin` print are
//! read off a terminal and typed or pasted into a shell, a line at a time.
//! Single quotes carry every byte but one through `sh` — and a line cannot
//! carry a newline at all: a path holding one prints across two lines, and
//! pasted, or retyped from the screen, it is two commands. Other control
//! characters (a carriage return, an escape) rewrite the very terminal line
//! the reader copies from. `$'…'` would spell them, and is not POSIX `sh`.
//!
//! So a command that would name such a path is not printed as a command at
//! all. What is printed instead says so, and shows the path with each
//! control character written out, as `rescue::printable` does for the
//! comments in `rescue.sh` (D68, D72). Pure: strings in, strings out.
//!
//! The same goes for a path that is not valid UTF-8 (D76). Printed, it is a
//! lossy copy with `U+FFFD` where its bytes were, and a command spelling the
//! copy names a different path — one that likely does not exist, or worse,
//! one that does. [`written_out`] shows such a path with `\xNN` for each of
//! those bytes, for reading only.

use std::path::Path;

use crate::rescue::printable;

/// `p` as one shell word: bare when it is made only of characters no shell
/// treats specially, single-quoted otherwise. Any control character is left
/// in as it is; [`word`] is the form that declines one.
pub fn quoted(p: &Path) -> String {
    let s = p.to_string_lossy();
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@%=:,".contains(c));
    if plain {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// [`quoted`], or `None` when no printed line can name `p`: it holds a
/// control character, or it is not valid UTF-8 (D76).
pub fn word(p: &Path) -> Option<String> {
    let s = p.to_str()?;
    (!has_control(s)).then(|| quoted(p))
}

/// Whether `s` holds a control character (`char::is_control`: C0, DEL, C1).
pub fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Printed where a command naming `p` would have been.
pub fn withheld(p: &Path) -> String {
    if p.to_str().is_none() {
        return format!(
            "(no command is printed for {}: its name is not valid UTF-8, and a command \
             printed here would name a different path. D76)",
            written_out(p)
        );
    }
    format!(
        "(no command is printed for {}: its name holds a control character, which a line \
         pasted into a shell cannot carry. D72)",
        written_out(p)
    )
}

/// `p` for reading, never for pasting: each control character written out
/// as [`printable`] does, and each byte that is not valid UTF-8 as `\xNN`,
/// so two different paths never print the same.
pub fn written_out(p: &Path) -> String {
    let mut out = String::new();
    for chunk in p.as_os_str().as_encoded_bytes().utf8_chunks() {
        out.push_str(&printable(chunk.valid()));
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// `line`, a whole printed command, when it is safe to print; otherwise the
/// `#` comment lines that replace it, with the command shown escaped. For
/// `doctor`, whose commands are built from paths first and checked here,
/// once, as they are laid out.
///
/// A path that is not valid UTF-8 reaches a built command through
/// [`quoted`] as `U+FFFD`, the replacement character, which stands for bytes
/// the path really has and the line does not: pasted, it names another
/// path. So a line holding one is left out too (D76). A path that really
/// holds the character is left out with it; on a screen the two cannot be
/// told apart, which is the problem.
pub fn command_lines(line: &str) -> Vec<String> {
    if has_control(line) {
        return vec![
            "# left out: a path in this command holds a control character, and a line".into(),
            "# pasted into a shell cannot carry one (D72). written out, it is:".into(),
            format!("#   {}", printable(line)),
        ];
    }
    if line.contains(char::REPLACEMENT_CHARACTER) {
        return vec![
            "# left out: a path in this command is not valid UTF-8, and the command as".into(),
            "# printed would name a different path (D76). with the bytes it cannot show".into(),
            "# replaced by U+FFFD, it is:".into(),
            format!("#   {line}"),
        ];
    }
    vec![line.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_bare_special_quoted_control_declined() {
        assert_eq!(
            word(Path::new("/home/u/.config/foot")).unwrap(),
            "/home/u/.config/foot"
        );
        assert_eq!(
            word(Path::new("/home/u/my dir")).unwrap(),
            "'/home/u/my dir'"
        );
        assert_eq!(
            word(Path::new("/home/u/it's")).unwrap(),
            "'/home/u/it'\\''s'"
        );
        assert_eq!(word(Path::new("/home/u/a\nb")), None);
        assert_eq!(word(Path::new("/home/u/a\u{1b}[2Jb")), None);
    }

    #[test]
    fn a_path_that_is_not_utf8_is_declined_and_written_out_by_its_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let p = Path::new(OsStr::from_bytes(b"/home/u/caf\xe9 dir"));
        assert_eq!(word(p), None);
        assert_eq!(written_out(p), "/home/u/caf\\xe9 dir");
        let said = withheld(p);
        assert!(
            said.contains("caf\\xe9 dir") && said.contains("D76"),
            "{said}"
        );
        // Two paths that differ only in their invalid bytes are written out
        // differently, where the lossy form would print them the same.
        let q = Path::new(OsStr::from_bytes(b"/home/u/caf\xff dir"));
        assert_ne!(written_out(p), written_out(q));
        assert_eq!(p.to_string_lossy(), q.to_string_lossy());

        let line = format!("mv -nT {} /x", quoted(p));
        let lines = command_lines(&line);
        assert!(lines.iter().all(|l| l.starts_with('#')), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("D76")), "{lines:?}");
    }

    #[test]
    fn a_command_with_a_newline_becomes_comments_only() {
        let lines = command_lines("mv -nT '/home/u/a\necho hi' /x");
        assert!(lines
            .iter()
            .all(|l| l.starts_with('#') && !l.contains('\n')));
        assert!(lines[2].contains("a\\necho hi"));
        assert_eq!(command_lines("ls -la /x"), vec!["ls -la /x".to_string()]);
    }
}
