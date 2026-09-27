//! Hyprland's `conf` dialect, read exactly as far as the verify-config
//! sandbox needs and no further. **Pure**: no IO, no clock, no environment.
//!
//! `Hyprland --verify-config` parses a config twice and forks every `exec =`
//! line on the second pass (`NOT-POSSIBLE.md#verify-config-as-proof`). So
//! before it is pointed at anything, ricepilot makes a scratch copy in which
//! (D55):
//!
//! * every line whose keyword is in the exec family — `exec`, `execr`,
//!   `exec-once`, `execr-once`, `exec-shutdown`, and anything else starting
//!   with `exec` — is blanked, as is `plugin`, which loads a shared object;
//! * every `source =` line is resolved the way Hyprland would resolve it
//!   *after the switch*, the file it names is copied (and stripped) too, and
//!   the line is rewritten to name the copy;
//! * `$variable` definitions whose value is a path into the profile, or
//!   starts with `~`, are rewritten to point into the scratch copy.
//!
//! The IO — reading the files, listing directories for a globbed `source`,
//! writing the copy — is done by the only code allowed to build a
//! [`crate::ops::exec::SandboxedConfig`], which calls [`sanitize`] once per
//! file and hands it a closure for each `source` it meets.
//!
//! # Erring towards stripping
//!
//! Where this module and Hyprland could disagree about what a line is, the
//! reading chosen is the one under which *more* is stripped. Blanking a line
//! Hyprland would not have run costs a less complete syntax check; keeping a
//! line Hyprland would have run costs a process the user never asked for. So
//! a line is stripped if its keyword looks like `exec…` after any leading
//! whitespace, `#`, byte-order mark or other non-alphanumeric noise, in any
//! case, in or out of a category — a commented-out `# exec-once = …` is
//! blanked too, harmlessly. Where the disagreement could instead make a
//! `source` line go unrewritten (and so read, unstripped, from the live
//! filesystem), the config is not sandboxed at all: see [`Unsandboxable`].
//!
//! Stripped lines are replaced by empty lines, never removed, so every line
//! number Hyprland reports about the copy is a line number in the original.
//!
//! # Lines are physical lines, because continued ones are refused
//!
//! hyprlang joins a line ending in `\` to the next one before it reads a
//! keyword, so `ex\` followed by `ec = …` is an `exec` line that no single
//! physical line spells. Rather than model the join — when it happens
//! relative to comment removal, whether trailing space counts, what `\\`
//! means — any file with a line whose text, or whose code before a comment,
//! ends in `\` is not sandboxed at all ([`Unsandboxable::Continued`], D67).
//! Every other rule here can then treat one physical line as one line.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Keywords stripped besides the `exec` family. `plugin =` names a shared
/// object to load; whether `--verify-config` loads it is not something
/// ricepilot can see, so it does not find out.
pub const ALSO_STRIPPED: &[&str] = &["plugin"];

/// A glob metacharacter ricepilot's own matcher does not implement. A
/// `source` using one is not sandboxed rather than approximated.
const UNSUPPORTED_GLOB: &[char] = &['[', ']', '{', '}'];

/// The fewest facts about the switch the rewriting needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The home directory `~` means.
    pub home: PathBuf,
    /// Where the post-switch filesystem is mirrored. A path `V` as Hyprland
    /// would see it after the switch lives at `mirror/V`.
    pub mirror: PathBuf,
    /// The links the switch will leave in place, as `(dest, src)`.
    pub links: Vec<(PathBuf, PathBuf)>,
    /// Destinations the switch will leave empty (retired, D36).
    pub absent: Vec<PathBuf>,
}

/// Where the thing Hyprland will see at `v` after the switch is *now*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Real {
    /// Read it from here.
    At(PathBuf),
    /// Nothing will be there after the switch.
    Absent,
}

impl Context {
    /// `v` inside the mirror.
    pub fn mirrored(&self, v: &Path) -> PathBuf {
        self.mirror.join(v.strip_prefix("/").unwrap_or(v))
    }

    /// Where to read what `v` will be after the switch: under a managed
    /// destination that means under its new source, never through the link
    /// that is there now — which still points at the profile being left.
    pub fn real(&self, v: &Path) -> Real {
        if let Some((dest, src)) = self
            .links
            .iter()
            .filter(|(d, _)| v.starts_with(d))
            .max_by_key(|(d, _)| d.components().count())
        {
            return Real::At(src.join(v.strip_prefix(dest).unwrap_or(Path::new(""))));
        }
        if self.absent.iter().any(|d| v.starts_with(d)) {
            return Real::Absent;
        }
        Real::At(v.to_path_buf())
    }

    /// A path written in a config, as Hyprland will see it after the switch:
    /// a path into a profile's source tree is the same file reached through
    /// its destination.
    pub fn virtualize(&self, p: &Path) -> PathBuf {
        match self
            .links
            .iter()
            .filter(|(_, s)| p.starts_with(s))
            .max_by_key(|(_, s)| s.components().count())
        {
            Some((dest, src)) => dest.join(p.strip_prefix(src).unwrap_or(Path::new(""))),
            None => p.to_path_buf(),
        }
    }
}

/// Why a config was not sandboxed, and so not verified. Each is a refusal
/// (`Refusal::VerifyConfigFailed`): a config ricepilot cannot copy faithfully
/// is one it cannot check, and a check that could not be done is not passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsandboxable {
    /// A keyword containing `$` somewhere other than its start, or a line
    /// with no `=` that mentions a variable: whether Hyprland would expand it
    /// into a keyword — `exec-once`, say — is not something to guess about.
    VariableKeyword { file: PathBuf, line: usize },
    /// A line ending in `\` — in its text, or in its code before a comment —
    /// which hyprlang may join to the next line, so that no physical line
    /// shows the keyword Hyprland reads (D67).
    Continued { file: PathBuf, line: usize },
    /// A `$name` in a `source` path that is neither a variable defined
    /// earlier in the config nor `$HOME`.
    UnknownVariable {
        file: PathBuf,
        line: usize,
        name: String,
    },
    /// A `source` path ricepilot will not resolve: `~user`, `..`, a glob
    /// class, or characters the rewritten line could not carry.
    Path {
        file: PathBuf,
        line: usize,
        path: String,
        why: &'static str,
    },
    /// Found while reading or copying: a file that sources itself, too many
    /// files, a file too big, a glob matching a directory, an unreadable
    /// file, a file sourced twice to different effect.
    Copy { path: PathBuf, why: String },
}

impl Unsandboxable {
    /// The same reason, naming `real` where it named `seen_as` — so what the
    /// user is told points at the file they would edit, not at the path it
    /// will have after the switch.
    pub fn reported_at(mut self, seen_as: &Path, real: &Path) -> Self {
        match &mut self {
            Unsandboxable::VariableKeyword { file, .. }
            | Unsandboxable::Continued { file, .. }
            | Unsandboxable::UnknownVariable { file, .. }
            | Unsandboxable::Path { file, .. } => {
                if file == seen_as {
                    *file = real.to_path_buf();
                }
            }
            Unsandboxable::Copy { .. } => {}
        }
        self
    }
}

impl fmt::Display for Unsandboxable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = |file: &Path, line: &usize| format!("{} line {line}", file.display());
        match self {
            Unsandboxable::VariableKeyword { file, line } => write!(
                f,
                "{}: a keyword or line built from a variable. ricepilot cannot tell whether it \
                 would expand to `exec`, so it did not run Hyprland on it",
                at(file, line)
            ),
            Unsandboxable::Continued { file, line } => write!(
                f,
                "{}: the line ends in `\\`, which Hyprland may join to the next line. \
                 ricepilot reads a config one line at a time to strip `exec`, and will not \
                 run Hyprland on a config with a continued line anywhere in it (D67)",
                at(file, line)
            ),
            Unsandboxable::UnknownVariable { file, line, name } => write!(
                f,
                "{}: `source` uses `${name}`, which is not a variable defined before it (nor \
                 `$HOME`), so ricepilot cannot tell which file Hyprland would read",
                at(file, line)
            ),
            Unsandboxable::Path {
                file,
                line,
                path,
                why,
            } => write!(f, "{}: `source = {path}`: {why}", at(file, line)),
            Unsandboxable::Copy { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

/// A `source =` line, resolved to the path — possibly a glob — Hyprland
/// would read after the switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRequest {
    /// Absolute, `~` and variables expanded, `.` removed, no `..`.
    pub pattern: PathBuf,
    /// The file it was found in (as seen after the switch) and its line.
    pub file: PathBuf,
    pub line: usize,
}

/// One blanked line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    pub line: usize,
    pub keyword: String,
}

/// A file, sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sanitized {
    pub text: String,
    pub stripped: Vec<Stripped>,
}

/// Variables in the order Hyprland defines them, across every file: a
/// variable defined in a sourced file is visible after the `source` line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vars(Vec<(String, String)>);

impl Vars {
    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn set(&mut self, name: &str, value: String) {
        self.0.push((name.to_string(), value));
    }
}

/// The part of a physical line before its comment. `##` is an escaped `#`.
pub fn code_of(line: &str) -> &str {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            if b.get(i + 1) == Some(&b'#') {
                i += 2;
                continue;
            }
            return &line[..i];
        }
        i += 1;
    }
    line
}

/// Leading noise a keyword is looked for behind: whitespace, `#`, a
/// byte-order mark, anything not ASCII alphanumeric — except `$`, which
/// starts a variable definition rather than a keyword.
fn denoise(s: &str) -> &str {
    s.trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != '$')
}

/// The run of `[A-Za-z0-9_.:-]` a (denoised) line starts with.
fn word_of(s: &str) -> &str {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || "_.:-".contains(c)))
        .unwrap_or(s.len());
    &s[..end]
}

/// The last `:`-separated segment of a keyword, which is what names it:
/// `general:exec` is compared as `exec`.
fn last_segment(word: &str) -> &str {
    word.rsplit(':').next().unwrap_or("")
}

/// The keyword a physical line would be read as, if it is one ricepilot
/// strips — erring towards finding one (see the module docs).
///
/// The whole line is looked at, comment included, behind any leading noise
/// ([`denoise`]); the keyword's last segment is compared case-insensitively.
/// So `exec-once`, `EXEC-ONCE`, `exec=`, `\texecr-once`, `# exec-once` and
/// `general:exec` are all found, and so is any keyword Hyprland adds later
/// that starts with `exec`.
pub fn stripped_keyword(line: &str) -> Option<String> {
    let d = denoise(line);
    if d.starts_with('$') {
        return None;
    }
    let word = word_of(d);
    let last = last_segment(word).to_ascii_lowercase();
    if last.starts_with("exec") || ALSO_STRIPPED.contains(&last.as_str()) {
        return Some(word.to_string());
    }
    None
}

/// Whether a physical line may continue onto the next. Either reading
/// counts: the `\` at the end of the code, or at the end of the whole line,
/// trailing whitespace (a `\r` included) ignored either way.
pub fn continues(line: &str) -> bool {
    line.trim_end().ends_with('\\') || code_of(line).trim_end().ends_with('\\')
}

/// A line's keyword and the byte span of its value, if it is `key = value`.
struct KeyValue<'a> {
    key: &'a str,
    /// Byte offsets into the line of the value, trimmed.
    value: (usize, usize),
}

/// Read from the code only, so a commented-out `# source = …` is never made
/// live; the key is denoised, so a `source` behind a byte-order mark is
/// still found.
fn key_value(line: &str) -> Option<KeyValue<'_>> {
    let code = code_of(line);
    let eq = code.find('=')?;
    let key = denoise(code[..eq].trim());
    let raw = &code[eq + 1..];
    let lead = raw.len() - raw.trim_start().len();
    let start = eq + 1 + lead;
    let end = eq + 1 + raw.trim_end().len();
    Some(KeyValue {
        key,
        value: (start, end.max(start)),
    })
}

/// Sanitize one file.
///
/// `file` is the path Hyprland will see it at after the switch (relative
/// `source` paths are resolved against its directory). `on_source` is called
/// for each `source =`, in order, before the rest of the file is read — so a
/// variable the sourced file defines is defined for the lines after it, as it
/// is in Hyprland.
pub fn sanitize(
    text: &str,
    file: &Path,
    ctx: &Context,
    vars: &mut Vars,
    on_source: &mut dyn FnMut(&SourceRequest, &mut Vars) -> Result<(), Unsandboxable>,
) -> Result<Sanitized, Unsandboxable> {
    let lines: Vec<&str> = text.split('\n').collect();
    // Before anything else — before a `source` is followed — so a file with a
    // continued line anywhere in it is refused whole (D67).
    if let Some(i) = lines.iter().position(|l| continues(l)) {
        return Err(Unsandboxable::Continued {
            file: file.to_path_buf(),
            line: i + 1,
        });
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut stripped = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;

        if let Some(keyword) = stripped_keyword(line) {
            stripped.push(Stripped { line: n, keyword });
            out.push(String::new());
            continue;
        }

        let Some(kv) = key_value(line) else {
            // No `=`: a category line, `}`, a blank or a comment. One that
            // uses a variable could be expanded into anything.
            if code_of(line).contains('$') {
                return Err(Unsandboxable::VariableKeyword {
                    file: file.to_path_buf(),
                    line: n,
                });
            }
            out.push((*line).to_string());
            continue;
        };
        let is_var = kv.key.starts_with('$');
        if !is_var && kv.key.contains('$') {
            return Err(Unsandboxable::VariableKeyword {
                file: file.to_path_buf(),
                line: n,
            });
        }
        let is_source = !is_var && last_segment(word_of(kv.key)).eq_ignore_ascii_case("source");
        if !(is_var || is_source) {
            out.push((*line).to_string());
            continue;
        }
        let value = &line[kv.value.0..kv.value.1];
        if is_var {
            let name = kv.key[1..].trim();
            vars.set(name, expand_vars(value, vars));
            let rewritten = rewrite_paths(value, ctx);
            out.push(format!(
                "{}{}{}",
                &line[..kv.value.0],
                rewritten,
                &line[kv.value.1..]
            ));
            continue;
        }

        // `source =`.
        let pattern = resolve_source(value, file, n, ctx, vars)?;
        let mirrored = ctx.mirrored(&pattern);
        let shown = mirrored.to_string_lossy();
        if shown.contains(['#', '$', '\n', '\r']) || shown.trim() != shown {
            return Err(Unsandboxable::Path {
                file: file.to_path_buf(),
                line: n,
                path: value.to_string(),
                why: "the scratch copy's path would contain `#`, `$` or surrounding space, \
                      which a rewritten `source =` line cannot carry",
            });
        }
        on_source(
            &SourceRequest {
                pattern,
                file: file.to_path_buf(),
                line: n,
            },
            vars,
        )?;
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        out.push(format!("{indent}source = {shown}"));
    }

    Ok(Sanitized {
        text: out.join("\n"),
        stripped,
    })
}

/// `$name` → its value, where it is defined. Used for variable values, where
/// an unknown name is left as written (Hyprland may know it; ricepilot only
/// needs the value to resolve a later `source`, which checks again).
fn expand_vars(value: &str, vars: &Vars) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        match vars.get(&name) {
            Some(v) if !name.is_empty() => out.push_str(v),
            _ => {
                out.push('$');
                out.push_str(&name);
            }
        }
        rest = &after[name.len()..];
    }
    out.push_str(rest);
    out
}

/// A `source` value → the absolute, post-switch path (or glob) Hyprland
/// would read.
fn resolve_source(
    value: &str,
    file: &Path,
    line: usize,
    ctx: &Context,
    vars: &Vars,
) -> Result<PathBuf, Unsandboxable> {
    let refuse = |why: &'static str| Unsandboxable::Path {
        file: file.to_path_buf(),
        line,
        path: value.to_string(),
        why,
    };

    // Variables: the longest run of name characters after each `$`, which
    // must be defined — or be `$HOME`, which Hyprland takes from the
    // environment and which, after the switch, is `ctx.home`.
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        expanded.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        match vars.get(&name) {
            Some(v) if !name.is_empty() => expanded.push_str(v),
            _ if name == "HOME" => expanded.push_str(&ctx.home.to_string_lossy()),
            _ => {
                return Err(Unsandboxable::UnknownVariable {
                    file: file.to_path_buf(),
                    line,
                    name,
                })
            }
        }
        rest = &after[name.len()..];
    }
    expanded.push_str(rest);
    let expanded = expanded.trim();

    if expanded.is_empty() {
        return Err(refuse("an empty path"));
    }
    if expanded.contains(UNSUPPORTED_GLOB) {
        return Err(refuse(
            "a glob with `[…]` or `{…}`, which ricepilot's matcher does not implement and \
             will not approximate",
        ));
    }

    let absolute = if expanded == "~" {
        ctx.home.clone()
    } else if let Some(r) = expanded.strip_prefix("~/") {
        ctx.home.join(r)
    } else if expanded.starts_with('~') {
        return Err(refuse("`~user` is not resolved"));
    } else if expanded.starts_with('/') {
        PathBuf::from(expanded)
    } else {
        // Relative to the directory of the file it is written in, which is
        // what Hyprland 0.55 does (observed, not documented).
        file.parent().unwrap_or(Path::new("/")).join(expanded)
    };

    let mut clean = PathBuf::from("/");
    for c in absolute.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(x) => clean.push(x),
            // Whether `..` is taken lexically or through a symlink changes the
            // answer — and the destination *is* a symlink after the switch.
            Component::ParentDir => {
                return Err(refuse(
                    "`..` would be resolved through the destination's symlink, which points \
                     somewhere else after the switch; ricepilot will not guess which way",
                ))
            }
            Component::Prefix(_) => return Err(refuse("not a Unix path")),
        }
    }
    Ok(ctx.virtualize(&clean))
}

/// Rewrite paths in a variable's value so they point into the scratch copy:
/// a leading `~`, and any absolute path under a destination or a source the
/// switch links. A path that merely shares a prefix — `~/.config/hyprpaper`
/// beside `~/.config/hypr` — is left alone.
pub fn rewrite_paths(value: &str, ctx: &Context) -> String {
    let mut prefixes: Vec<(String, PathBuf)> = Vec::new();
    for (dest, src) in &ctx.links {
        prefixes.push((dest.to_string_lossy().into_owned(), ctx.mirrored(dest)));
        prefixes.push((src.to_string_lossy().into_owned(), ctx.mirrored(dest)));
    }
    // Longest first, so a nested destination wins over its parent.
    prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    let home = ctx.mirrored(&ctx.home).to_string_lossy().into_owned();

    let starts_token = |s: &str, i: usize| {
        i == 0
            || s[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || "=,\"'(:".contains(c))
    };
    let ends_token = |s: &str| {
        s.chars()
            .next()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || "._-".contains(c)))
    };

    let mut out = String::new();
    let mut i = 0;
    while i < value.len() {
        let rest = &value[i..];
        if starts_token(value, i) {
            if rest.starts_with('~') && ends_token(&rest[1..]) {
                out.push_str(&home);
                i += 1;
                continue;
            }
            if let Some((p, to)) = prefixes
                .iter()
                .find(|(p, _)| rest.starts_with(p.as_str()) && ends_token(&rest[p.len()..]))
            {
                out.push_str(&to.to_string_lossy());
                i += p.len();
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Whether a physical line of an *already sanitized* file is a `source`
/// line, and if so its value. Used by the post-check that every `source`
/// left in a scratch copy names a file inside it.
pub fn source_value(line: &str) -> Option<&str> {
    let kv = key_value(line)?;
    (!kv.key.starts_with('$') && last_segment(word_of(kv.key)).eq_ignore_ascii_case("source"))
        .then(|| &line[kv.value.0..kv.value.1])
}

/// Whether a glob component matches a directory entry's name, as glob(3)
/// does it: `*` and `?`, and a leading `.` only matched by a leading `.`.
pub fn glob_component_matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    crate::verify::glob_matches(pattern, name)
}

/// Whether a path component is a glob.
pub fn is_glob(component: &str) -> bool {
    component.contains(['*', '?'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            home: "/home/u".into(),
            mirror: "/home/u/.local/state/ricepilot/verify/ID/root".into(),
            links: vec![
                ("/home/u/.config/hypr".into(), "/rice/new/hypr".into()),
                ("/home/u/.config/foot".into(), "/rice/new/foot".into()),
            ],
            absent: vec!["/home/u/.config/btop".into()],
        }
    }

    const M: &str = "/home/u/.local/state/ricepilot/verify/ID/root";

    fn run(text: &str) -> (Sanitized, Vec<SourceRequest>) {
        let mut seen = Vec::new();
        let s = sanitize(
            text,
            Path::new("/home/u/.config/hypr/hyprland.conf"),
            &ctx(),
            &mut Vars::default(),
            &mut |r, _| {
                seen.push(r.clone());
                Ok(())
            },
        )
        .unwrap();
        (s, seen)
    }

    fn refused(text: &str) -> Unsandboxable {
        sanitize(
            text,
            Path::new("/home/u/.config/hypr/hyprland.conf"),
            &ctx(),
            &mut Vars::default(),
            &mut |_, _| Ok(()),
        )
        .unwrap_err()
    }

    /// Every spelling of an exec-family line, including the ones Hyprland
    /// itself would not accept: stripping one of those costs nothing.
    #[test]
    fn every_exec_spelling_is_stripped() {
        let nasty = [
            "exec = touch /x",
            "exec-once = touch /x",
            "execr = touch /x",
            "execr-once = touch /x",
            "exec-shutdown = touch /x",
            "exec=touch /x",
            "exec-once=touch /x",
            "   exec-once   =   touch /x",
            "\texec-once\t=\ttouch /x",
            "EXEC-ONCE = touch /x",
            "Exec = touch /x",
            "exec-once = touch /x # with a comment",
            "exec-once = touch /x ## not a comment # a comment",
            "# exec-once = touch /x",
            "## exec-once = touch /x",
            "\u{feff}exec-once = touch /x",
            "\u{a0}exec-once = touch /x",
            "\u{200b}exec-once = touch /x",
            "general:exec = touch /x",
            "  exec-once = touch /x\r",
            "exec-something-new = touch /x",
            "exec {",
            "plugin = /usr/lib/evil.so",
            "PLUGIN=/usr/lib/evil.so",
            // Not a keyword Hyprland has. Stripped anyway: anything that
            // starts with `exec` is.
            "executable_path_hint = 1",
        ];
        for line in nasty {
            let (s, _) = run(line);
            assert_eq!(s.text, "", "{line:?} survived as {:?}", s.text);
            assert_eq!(s.stripped.len(), 1, "{line:?}");
        }
    }

    /// Lines that mention exec but are not exec lines are kept as written.
    #[test]
    fn lines_that_only_mention_exec_are_kept() {
        let kept = [
            "bind = SUPER, Q, exec, foot",
            "bind = SUPER, E, exec, exec-once-helper",
            "$exec = foot",
            "$execs = foot # a variable named exec is not a keyword",
            "windowrule = float, class:exec",
            "workspace = 1, on-created-empty:foot",
            "general {",
            "}",
            "",
            "# a comment",
            "# source = /etc/never-made-live.conf",
        ];
        for line in kept {
            let (s, seen) = run(line);
            assert!(seen.is_empty(), "{line:?} was read as a source");
            assert_eq!(s.text, line, "{line:?}");
            assert!(s.stripped.is_empty(), "{line:?}");
        }
    }

    /// Stripped lines become empty lines, so every line number Hyprland
    /// reports about the copy is the original's.
    #[test]
    fn line_numbers_survive_stripping() {
        let (s, _) = run("a = 1\nexec-once = x\nb = 2\n");
        assert_eq!(s.text, "a = 1\n\nb = 2\n");
        assert_eq!(s.stripped[0].line, 2);
    }

    /// hyprlang joins a line ending in `\\` to the next, which can build a
    /// keyword no physical line spells. Any such line, anywhere in the file,
    /// in either reading, is refused before a single `source` is followed —
    /// and it names the first continued line (D67).
    #[test]
    fn any_continued_line_refuses_the_whole_file() {
        let cases = [
            // The attack: `exec` split across the join.
            ("ex\\\nec = touch /x\n", 1),
            ("e\\\nx\\\nec-once = touch /x\n", 1),
            // Hidden behind a value, a comment, trailing space, a CR.
            ("keep = 1\nbind = a, \\\nexec-once = x\n", 2),
            ("a = 1 \\ # a comment\nexec = x\n", 1),
            ("# just a comment \\\nexec = x\n", 1),
            ("a = 1 \\   \nb = 2\n", 1),
            ("a = 1 \\\r\nb = 2\r\n", 1),
            ("a = 1 \\\\\nb = 2\n", 1),
            ("exec-once = one \\\n  two\n", 1),
            ("source = ~/.config/hypr/a.conf \\\n  more\n", 1),
            // At the very end, with nothing to join to: refused all the same.
            ("a = 1\nb = 2 \\", 2),
        ];
        for (text, line) in cases {
            let mut followed = 0;
            let got = sanitize(
                &format!("source = /etc/first.conf\n{text}"),
                Path::new("/home/u/.config/hypr/hyprland.conf"),
                &ctx(),
                &mut Vars::default(),
                &mut |_, _| {
                    followed += 1;
                    Ok(())
                },
            );
            match got {
                Err(Unsandboxable::Continued { line: l, ref file }) => {
                    assert_eq!(l, line + 1, "{text:?}");
                    assert_eq!(file, Path::new("/home/u/.config/hypr/hyprland.conf"));
                }
                other => panic!("{text:?} was not refused as continued: {other:?}"),
            }
            assert_eq!(
                followed, 0,
                "{text:?}: a source was followed before refusing"
            );
        }
        // A backslash that does not end the line is not a continuation.
        let (s, _) = run("bind = SUPER, B, exec, a\\b\n");
        assert_eq!(s.text, "bind = SUPER, B, exec, a\\b\n");
    }

    /// A variable can only be expanded where it cannot become a keyword:
    /// in a value. A keyword or a line with no `=` that uses one is refused.
    #[test]
    fn a_variable_where_a_keyword_could_be_is_refused() {
        for (text, line) in [
            ("foo$x = 1\n", 1),
            ("$cmd = exec-once = touch /x\n$cmd\n", 2),
            ("  $cmd  \n", 1),
            ("$cat {\n}\n", 1),
            ("general:$k = 1\n", 1),
        ] {
            match refused(text) {
                Unsandboxable::VariableKeyword { line: l, .. } => assert_eq!(l, line, "{text:?}"),
                other => panic!("{text:?}: {other:?}"),
            }
        }
        // In a value, or in a comment, it is fine.
        let (s, _) = run("$t = foot\nbind = SUPER, T, exec, $t\n# uses $t\n");
        assert!(s.stripped.is_empty());
    }

    #[test]
    fn source_is_rewritten_into_the_mirror_and_requested() {
        let (s, seen) = run("source = ~/.config/hypr/a.conf # tail\n  source=./b.conf\n");
        assert_eq!(
            s.text,
            format!(
                "source = {M}/home/u/.config/hypr/a.conf\n  source = {M}/home/u/.config/hypr/b.conf\n"
            )
        );
        assert_eq!(seen[0].pattern, Path::new("/home/u/.config/hypr/a.conf"));
        assert_eq!(seen[1].pattern, Path::new("/home/u/.config/hypr/b.conf"));
        assert_eq!(seen[1].line, 2);
    }

    /// A path written into the profile's *source* tree is the same file seen
    /// through its destination after the switch.
    #[test]
    fn a_source_into_the_profile_tree_is_seen_through_its_destination() {
        let (_, seen) = run("source = /rice/new/hypr/conf.d/x.conf\n");
        assert_eq!(
            seen[0].pattern,
            Path::new("/home/u/.config/hypr/conf.d/x.conf")
        );
    }

    /// A `source` behind noise Hyprland might skip is still found and
    /// rewritten; a `source` inside a category block is still a `source`.
    #[test]
    fn a_source_in_disguise_is_still_rewritten() {
        let (s, seen) = run("\u{feff}source = /etc/x.conf\ngeneral {\n\tSOURCE=/etc/y.conf\n}\nx:source = /etc/z.conf");
        assert_eq!(seen.len(), 3);
        assert_eq!(
            s.text,
            format!("source = {M}/etc/x.conf\ngeneral {{\n\tsource = {M}/etc/y.conf\n}}\nsource = {M}/etc/z.conf")
        );
    }

    #[test]
    fn globbed_source_keeps_its_glob() {
        let (s, seen) = run("source = ~/.config/hypr/conf.d/*.conf\n");
        assert_eq!(
            s.text,
            format!("source = {M}/home/u/.config/hypr/conf.d/*.conf\n")
        );
        assert_eq!(
            seen[0].pattern,
            Path::new("/home/u/.config/hypr/conf.d/*.conf")
        );
    }

    #[test]
    fn variables_resolve_sources_and_are_rewritten() {
        let text = "$hypr = ~/.config/hypr\n$conf = $hypr/conf.d\nsource = $conf/x.conf\n\
                    source = $HOME/.config/foot/f.conf\n";
        let (s, seen) = run(text);
        let lines: Vec<&str> = s.text.lines().collect();
        assert_eq!(lines[0], format!("$hypr = {M}/home/u/.config/hypr"));
        // `$conf`'s value names only a variable, so there is nothing to
        // rewrite in it: Hyprland expands `$hypr`, which already points in.
        assert_eq!(lines[1], "$conf = $hypr/conf.d");
        assert_eq!(
            seen[0].pattern,
            Path::new("/home/u/.config/hypr/conf.d/x.conf")
        );
        assert_eq!(seen[1].pattern, Path::new("/home/u/.config/foot/f.conf"));
    }

    #[test]
    fn a_variable_defined_in_a_sourced_file_is_visible_after_it() {
        let mut vars = Vars::default();
        let mut seen = Vec::new();
        sanitize(
            "source = ./vars.conf\nsource = $dir/x.conf\n",
            Path::new("/home/u/.config/hypr/hyprland.conf"),
            &ctx(),
            &mut vars,
            &mut |r, v| {
                if r.pattern.ends_with("vars.conf") {
                    v.set("dir", "/home/u/.config/hypr/sub".into());
                }
                seen.push(r.pattern.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen[1], Path::new("/home/u/.config/hypr/sub/x.conf"));
    }

    #[test]
    fn variable_paths_rewrite_only_at_a_path_boundary() {
        let c = ctx();
        assert_eq!(
            rewrite_paths("/home/u/.config/hyprpaper/x", &c),
            "/home/u/.config/hyprpaper/x"
        );
        assert_eq!(
            rewrite_paths("/home/u/.config/hypr", &c),
            format!("{M}/home/u/.config/hypr")
        );
        assert_eq!(
            rewrite_paths("foo,/rice/new/hypr/s.sh bar", &c),
            format!("foo,{M}/home/u/.config/hypr/s.sh bar")
        );
        assert_eq!(rewrite_paths("~", &c), format!("{M}/home/u"));
        assert_eq!(rewrite_paths("~user/x", &c), "~user/x");
        assert_eq!(rewrite_paths("a~/b", &c), "a~/b");
        assert_eq!(rewrite_paths("/elsewhere/x", &c), "/elsewhere/x");
    }

    #[test]
    fn what_is_not_sandboxed() {
        assert!(matches!(
            refused("foo$x = 1\n"),
            Unsandboxable::VariableKeyword { line: 1, .. }
        ));
        assert!(matches!(
            refused("bind = a \\\nsource = /etc/x.conf\n"),
            Unsandboxable::Continued { line: 1, .. }
        ));
        assert!(matches!(
            refused("source = $nope/x.conf\n"),
            Unsandboxable::UnknownVariable { ref name, .. } if name == "nope"
        ));
        assert!(matches!(
            refused("source = $XDG_CONFIG_HOME/hypr/x.conf\n"),
            Unsandboxable::UnknownVariable { .. }
        ));
        for bad in [
            "source = ../x.conf",
            "source = ~/.config/hypr/../x.conf",
            "source = ~root/x.conf",
            "source = ~/.config/hypr/[ab].conf",
            "source = ~/.config/hypr/{a,b}.conf",
            "source =",
        ] {
            assert!(
                matches!(refused(bad), Unsandboxable::Path { .. }),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn real_follows_the_post_switch_links() {
        let c = ctx();
        assert_eq!(
            c.real(Path::new("/home/u/.config/hypr/a.conf")),
            Real::At("/rice/new/hypr/a.conf".into())
        );
        assert_eq!(c.real(Path::new("/home/u/.config/btop/x")), Real::Absent);
        assert_eq!(
            c.real(Path::new("/home/u/.config/caelestia/x.conf")),
            Real::At("/home/u/.config/caelestia/x.conf".into())
        );
    }

    #[test]
    fn source_value_finds_every_source_spelling() {
        assert_eq!(source_value("source = /a"), Some("/a"));
        assert_eq!(source_value("  SOURCE=/a # c"), Some("/a"));
        assert_eq!(source_value("x:source = /a"), Some("/a"));
        assert_eq!(source_value("sourced = /a"), None);
        assert_eq!(source_value("bind = a, source"), None);
        assert_eq!(source_value("# source = /a"), None);
        assert_eq!(source_value("\u{feff}source = /a"), Some("/a"));
        assert_eq!(source_value("$source = /a"), None);
    }

    #[test]
    fn glob_components_follow_glob3() {
        assert!(glob_component_matches("*.conf", "a.conf"));
        assert!(!glob_component_matches("*.conf", ".hidden.conf"));
        assert!(glob_component_matches(".*.conf", ".hidden.conf"));
        assert!(glob_component_matches("?.conf", "a.conf"));
        assert!(!glob_component_matches("*.conf", "a.conf.bak"));
    }
}
