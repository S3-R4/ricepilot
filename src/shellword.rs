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

/// [`quoted`], or `None` when `p` holds a control character and no printed
/// line can carry it.
pub fn word(p: &Path) -> Option<String> {
    (!has_control(&p.to_string_lossy())).then(|| quoted(p))
}

/// Whether `s` holds a control character (`char::is_control`: C0, DEL, C1).
pub fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Printed where a command naming `p` would have been.
pub fn withheld(p: &Path) -> String {
    format!(
        "(no command is printed for {}: its name holds a control character, which a line \
         pasted into a shell cannot carry. D72)",
        printable(&p.to_string_lossy())
    )
}

/// `line`, a whole printed command, when it is safe to print; otherwise the
/// `#` comment lines that replace it, with the command shown escaped. For
/// `doctor`, whose commands are built from paths first and checked here,
/// once, as they are laid out.
pub fn command_lines(line: &str) -> Vec<String> {
    if !has_control(line) {
        return vec![line.to_string()];
    }
    vec![
        "# left out: a path in this command holds a control character, and a line".into(),
        "# pasted into a shell cannot carry one (D72). written out, it is:".into(),
        format!("#   {}", printable(line)),
    ]
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
    fn a_command_with_a_newline_becomes_comments_only() {
        let lines = command_lines("mv -nT '/home/u/a\necho hi' /x");
        assert!(lines
            .iter()
            .all(|l| l.starts_with('#') && !l.contains('\n')));
        assert!(lines[2].contains("a\\necho hi"));
        assert_eq!(command_lines("ls -la /x"), vec!["ls -la /x".to_string()]);
    }
}
