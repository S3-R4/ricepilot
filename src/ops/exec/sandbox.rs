//! The one constructor of [`SandboxedConfig`], and therefore the only way
//! `Hyprland --verify-config` can be pointed at anything (D55).
//!
//! It lives inside `ops::exec` because it has to: `SandboxedConfig`'s fields
//! are private to that module and its children, so no other code can make
//! one, and this is the child that does. What it guarantees about the value
//! it returns:
//!
//! * the config, and every file it `source`s, has been **copied** into a
//!   fresh scratch directory — nothing Hyprland is given is the user's own
//!   file, and nothing is written anywhere but that directory;
//! * in every copy, each exec-family line (and `plugin`) has been blanked,
//!   and a **post-check re-reads every file written** and refuses to return
//!   a value if one survived, or if any `source` in a copy names a file
//!   outside it ([`crate::hyprconf`] does the rewriting; this checks it);
//! * `$variable` paths into the profile and a leading `~` point into the
//!   copy;
//! * the scratch directory is on the same device as `home`, so it is never
//!   `/tmp`'s tmpfs, and it is not under `/tmp` at all;
//! * the child it is handed to gets `HOME`, `XDG_*` and `XDG_RUNTIME_DIR`
//!   inside the scratch directory and **no** `HYPRLAND_INSTANCE_SIGNATURE`
//!   ([`super::Call::fixed_env`], and `env_clear` before it).
//!
//! It reads the live filesystem through [`read`] only — `O_PATH|O_NOFOLLOW`,
//! a symlinked component refused (D9) — and it reads what each path *will*
//! be after the switch: under a managed destination, from the new profile's
//! source, never through the link that is there now.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::SandboxedConfig;
use crate::hyprconf::{self, Context, Real, SourceRequest, Stripped, Unsandboxable, Vars};
use crate::ops::{mutate, read};
use crate::{Error, Result};

/// More files than any real config sources; enough that a glob gone wrong is
/// a refusal rather than a copy of somebody's home directory.
pub const MAX_FILES: usize = 256;
/// Larger than any real config file.
pub const MAX_BYTES: u64 = 1 << 20;
/// Deeper than any real chain of `source`s.
pub const MAX_DEPTH: usize = 32;

/// What to sandbox.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub home: &'a Path,
    /// The config's path as Hyprland will see it after the switch —
    /// `~/.config/hypr/hyprland.conf`.
    pub entry: &'a Path,
    /// `(dest, src)` for every link the switch leaves in place.
    pub links: &'a [(PathBuf, PathBuf)],
    /// Destinations the switch leaves empty.
    pub absent: &'a [PathBuf],
    /// The directory scratch copies are made in (`state/verify`), and the
    /// name to make this one under. A taken name gets a `-N` suffix.
    pub parent: &'a Path,
    pub id: &'a str,
}

/// One file of the copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Where it was read from.
    pub real: PathBuf,
    /// Where it was written.
    pub copy: PathBuf,
    pub stripped: Vec<Stripped>,
}

/// State of the depth-first walk. `ctx` is kept out of it so the closure
/// handed to [`hyprconf::sanitize`] can borrow this mutably.
#[derive(Default)]
struct Walk {
    stack: Vec<PathBuf>,
    /// By post-switch path: the real file, the sanitized text, what was
    /// stripped.
    files: BTreeMap<PathBuf, (PathBuf, String, Vec<Stripped>)>,
    visits: usize,
}

impl SandboxedConfig {
    /// Build the scratch copy, or say why it cannot be built.
    ///
    /// `Ok(Err(_))` is a config ricepilot will not sandbox — and so will not
    /// run Hyprland on — with nothing written. `Err(_)` is a failure writing
    /// the copy, or the post-check finding the copy is not what it must be.
    pub fn build(req: &Request<'_>) -> Result<std::result::Result<Self, Unsandboxable>> {
        where_scratch_may_go(req)?;

        // Chosen before anything is read, written only once everything has
        // been: a config that cannot be sandboxed leaves nothing behind.
        let scratch = free_name(req.parent, req.id)?;
        let ctx = Context {
            home: req.home.to_path_buf(),
            mirror: scratch.join("root"),
            links: req.links.to_vec(),
            absent: req.absent.to_vec(),
        };

        let mut walk = Walk::default();
        let mut vars = Vars::default();
        if let Err(why) = visit(&ctx, &mut walk, req.entry, &mut vars) {
            return Ok(Err(why));
        }

        // ---- Everything was read and sanitized. Now, and only now, write. ----
        if !mutate::make_dir_new(&scratch)? {
            return Err(refused(
                &scratch,
                "was created by something else between choosing its name and making it",
            ));
        }
        let home = ctx.mirrored(req.home);
        let runtime = scratch.join("run");
        mutate::make_dirs(&home)?;
        mutate::make_dirs(&runtime)?;

        let mut copied = Vec::new();
        for (v, (real, text, stripped)) in &walk.files {
            let copy = ctx.mirrored(v);
            mutate::write_atomic(&copy, text.as_bytes())?;
            copied.push(Copied {
                real: real.clone(),
                copy,
                stripped: stripped.clone(),
            });
        }

        post_check(&ctx, &copied)?;

        Ok(Ok(SandboxedConfig {
            file: ctx.mirrored(req.entry),
            scratch,
            home,
            runtime,
            copied,
        }))
    }
}

/// The scratch directory must be on `home`'s device and not under `/tmp`.
fn where_scratch_may_go(req: &Request<'_>) -> Result<()> {
    let mirror_dev = read::dev_of_nearest_existing_ancestor(req.parent)?;
    let home_dev = read::dev_of_nearest_existing_ancestor(req.home)?;
    if !req.parent.is_absolute() || req.parent.starts_with("/tmp") || mirror_dev != home_dev {
        return Err(refused(
            req.parent,
            "is not on the home directory's filesystem. a verify-config scratch copy is made \
             only under ricepilot's state directory, never on /tmp",
        ));
    }
    Ok(())
}

fn refused(path: &Path, why: &str) -> Error {
    Error::Refused {
        rule: "R3",
        path: path.to_path_buf(),
        why: why.to_string(),
    }
}

/// `parent/id`, or `parent/id-N` for the first free `N`.
fn free_name(parent: &Path, id: &str) -> Result<PathBuf> {
    for n in 0..1000u32 {
        let name = if n == 0 {
            id.to_string()
        } else {
            format!("{id}-{n}")
        };
        let candidate = parent.join(name);
        if read::lstat_or_absent(&candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(refused(
        parent,
        "has no free scratch name after 1000 attempts",
    ))
}

/// A read failure while copying, as the reason the config was not sandboxed.
fn unreadable(path: &Path, e: Error) -> Unsandboxable {
    Unsandboxable::Copy {
        path: path.to_path_buf(),
        why: format!("could not be copied: {e}"),
    }
}

/// Copy one file (as `v` will be after the switch), and everything it
/// sources, depth first and in order.
fn visit(
    ctx: &Context,
    walk: &mut Walk,
    v: &Path,
    vars: &mut Vars,
) -> std::result::Result<(), Unsandboxable> {
    let copy_err = |why: String| Unsandboxable::Copy {
        path: v.to_path_buf(),
        why,
    };
    if walk.stack.iter().any(|p| p == v) {
        return Err(copy_err(
            "sources itself, directly or through another file".into(),
        ));
    }
    if walk.stack.len() >= MAX_DEPTH {
        return Err(copy_err(format!("is more than {MAX_DEPTH} `source`s deep")));
    }
    walk.visits += 1;
    if walk.visits > MAX_FILES {
        return Err(copy_err(format!(
            "would take the copy past {MAX_FILES} files"
        )));
    }

    let real = match ctx.real(v) {
        Real::At(r) => r,
        // Nothing will be there after the switch; Hyprland, reading the
        // copy, will find nothing there either and say so.
        Real::Absent => return Ok(()),
    };
    let meta = read::lstat_or_absent(&real).map_err(|e| unreadable(&real, e))?;
    let Some(meta) = meta else {
        return Ok(());
    };
    if meta.kind != read::Kind::File {
        return Err(Unsandboxable::Copy {
            path: real,
            why: "is sourced but is not a regular file. ricepilot does not follow a symlink \
                  to find a config, and a directory is not one"
                .into(),
        });
    }
    let size = read::size_of(&real).map_err(|e| unreadable(&real, e))?;
    if size > MAX_BYTES {
        return Err(Unsandboxable::Copy {
            path: real,
            why: format!("is {size} bytes, more than the {MAX_BYTES} a config is allowed"),
        });
    }
    let text = read::slurp(&real).map_err(|e| unreadable(&real, e))?;

    walk.stack.push(v.to_path_buf());
    let sanitized = hyprconf::sanitize(&text, v, ctx, vars, &mut |r, vars| {
        expand(ctx, walk, r, vars)
    });
    walk.stack.pop();
    let sanitized = sanitized.map_err(|e| e.reported_at(v, &real))?;

    if let Some((_, before, _)) = walk.files.get(v) {
        if *before != sanitized.text {
            return Err(copy_err(
                "is sourced twice and would read differently each time (a variable it uses \
                 changed in between). one copy of it cannot stand for both"
                    .into(),
            ));
        }
        return Ok(());
    }
    walk.files
        .insert(v.to_path_buf(), (real, sanitized.text, sanitized.stripped));
    Ok(())
}

/// Every file a `source` pattern matches, visited in the order glob(3)
/// would return them.
fn expand(
    ctx: &Context,
    walk: &mut Walk,
    req: &SourceRequest,
    vars: &mut Vars,
) -> std::result::Result<(), Unsandboxable> {
    let mut globbed = false;
    let mut candidates = vec![PathBuf::from("/")];
    for part in req.pattern.components().skip(1) {
        let part = part.as_os_str().to_string_lossy().into_owned();
        if !hyprconf::is_glob(&part) {
            for c in &mut candidates {
                c.push(&part);
            }
            continue;
        }
        globbed = true;
        let mut next = Vec::new();
        for c in &candidates {
            let Real::At(dir) = ctx.real(c) else { continue };
            match read::lstat_or_absent(&dir).map_err(|e| unreadable(&dir, e))? {
                Some(m) if m.kind == read::Kind::Dir => {}
                _ => continue,
            }
            for name in read::list_dir(&dir).map_err(|e| unreadable(&dir, e))? {
                let name = name.to_string_lossy().into_owned();
                if hyprconf::glob_component_matches(&part, &name) {
                    next.push(c.join(&name));
                }
            }
        }
        if next.len() > MAX_FILES {
            return Err(Unsandboxable::Copy {
                path: req.pattern.clone(),
                why: format!("matches more than {MAX_FILES} paths"),
            });
        }
        next.sort();
        candidates = next;
    }

    for c in candidates {
        if globbed {
            // A literal path Hyprland cannot read is its to report. A glob
            // match that is not a file would be copied as nothing and read as
            // something, so it is refused here instead.
            if let Real::At(r) = ctx.real(&c) {
                if let Some(m) = read::lstat_or_absent(&r).map_err(|e| unreadable(&r, e))? {
                    if m.kind != read::Kind::File {
                        return Err(Unsandboxable::Copy {
                            path: r,
                            why: format!(
                                "is matched by `source = {}` and is not a regular file",
                                req.pattern.display()
                            ),
                        });
                    }
                }
            }
        }
        visit(ctx, walk, &c, vars)?;
    }
    Ok(())
}

/// Re-read every file written and refuse to hand back a sandbox that is not
/// one. This does not trust [`hyprconf::sanitize`]: it applies the same
/// detector to what is actually on the disk, line by line.
fn post_check(ctx: &Context, copied: &[Copied]) -> Result<()> {
    let mirror = ctx.mirror.to_string_lossy().into_owned();
    for c in copied {
        let text = read::slurp(&c.copy)?;
        for (i, line) in text.split('\n').enumerate() {
            // `sanitize` refuses a file with any continued line (D67), so
            // every line here is read alone — as the checks below assume.
            if hyprconf::continues(line) {
                return Err(refused(
                    &c.copy,
                    &format!(
                        "line {} ends in `\\`, which Hyprland may join to the next line, \
                         after sanitizing. this is a bug in ricepilot, and Hyprland was not run",
                        i + 1
                    ),
                ));
            }
            if let Some(k) = hyprconf::stripped_keyword(line) {
                return Err(refused(
                    &c.copy,
                    &format!(
                        "line {} still reads as `{k}` after sanitizing. this is a bug in \
                         ricepilot, and Hyprland was not run",
                        i + 1
                    ),
                ));
            }
            if let Some(v) = hyprconf::source_value(line) {
                if !v.starts_with(mirror.as_str()) || v.contains(['$', '~']) {
                    return Err(refused(
                        &c.copy,
                        &format!(
                            "line {} sources `{v}`, outside the scratch copy. this is a bug in \
                             ricepilot, and Hyprland was not run",
                            i + 1
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The sandbox against a **fake** `Hyprland`: a POSIX sh stand-in under
/// `target/fixtures/` that does the thing that makes the real one dangerous —
/// it runs every exec-family line of the config it is given, and of every
/// file that config sources, twice — and records its argv, its environment
/// and each file it read. The real binary is never run here: in unit tests
/// [`super::locate`] cannot return it (D55).
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::hyprverify::{self, NotChecked, Outcome};
    use crate::ops::exec::test_support::use_fake_hyprland;
    use crate::plan::Target;

    const FAKE: &str = r#"#!/bin/sh
# Stand-in for `Hyprland --verify-config -c FILE`. Builtins only: the child
# runs with an empty PATH.
here=${0%/*}
: > "$here/argv"
for a in "$@"; do printf '%s\n' "$a" >> "$here/argv"; done
export -p > "$here/env"
cfg=
while [ $# -gt 0 ]; do
  if [ "$1" = -c ]; then cfg=$2; shift; fi
  shift
done
parse() {
  printf '%s\n' "$1" >> "$here/parsed"
  while IFS= read -r line || [ -n "$line" ]; do
    code=${line%%#*}
    key=${code%%=*}
    [ "$key" = "$code" ] && continue
    key=$(echo $key)
    val=${code#*=}
    case "$key" in
      exec|execr|exec-once|execr-once|exec-shutdown|EXEC-ONCE) ( eval "$val" ) ;;
      source) for f in $val; do [ -f "$f" ] && parse "$f"; done ;;
    esac
  done < "$1"
}
parse "$cfg"
parse "$cfg"
echo "======== Config parsing result:"
echo
if [ -f "$here/verdict-fail" ]; then
  echo "Config error in file $cfg at line 2: the stand-in was told to fail"
  exit 1
fi
echo "config ok"
"#;

    /// A fixture of its own per run: nothing under `src/` may remove the
    /// last run's, so each run gets a fresh directory beside it (D5, D18).
    struct Fx {
        root: PathBuf,
    }

    impl Fx {
        fn new(case: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/fixtures/m5/sandbox")
                .join(format!("{case}-{}-{nanos}", std::process::id()));
            mutate::make_dirs(&root.join("markers")).unwrap();
            mutate::make_dirs(&root.join("home/.config")).unwrap();
            mutate::make_dirs(&root.join("home/.local/state/ricepilot")).unwrap();
            Self { root }
        }
        fn p(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }
        fn home(&self) -> PathBuf {
            self.p("home")
        }
        fn state(&self) -> PathBuf {
            self.p("home/.local/state/ricepilot")
        }
        fn markers(&self) -> Vec<String> {
            read::list_dir(&self.p("markers"))
                .unwrap()
                .into_iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect()
        }
        /// Write `rel`, with `MARK`, `RICE` and `HOME` spelled out.
        fn file(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.p(rel);
            let body = body
                .replace("MARK", &self.p("markers").to_string_lossy())
                .replace("RICE", &self.p("rice").to_string_lossy())
                .replace("HOMEDIR", &self.home().to_string_lossy());
            mutate::write_atomic(&path, body.as_bytes()).unwrap();
            path
        }
        fn fake(&self) -> PathBuf {
            let path = self.file("fake/Hyprland", FAKE);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        fn targets(&self) -> Vec<Target> {
            vec![Target {
                dest: self.p("home/.config/hypr"),
                src: self.p("rice/new/hypr"),
            }]
        }
        fn check(&self) -> Option<Outcome> {
            hyprverify::check(&self.home(), &self.state(), "ID", &self.targets(), &[]).unwrap()
        }
        fn lines(&self, rel: &str) -> Vec<String> {
            match read::slurp(&self.p(rel)) {
                Ok(t) => t.lines().map(str::to_string).collect(),
                Err(_) => Vec::new(),
            }
        }
    }

    /// A rice whose config exec-s at every turn: in the entry file, in a
    /// relatively sourced file, in two files a glob finds, in a file sourced
    /// by the profile's absolute path, and in a file outside the profile —
    /// plus a live `~/.config/hypr` that still points at the *old* profile,
    /// whose files exec too and must never be read.
    fn nasty(fx: &Fx) {
        fx.file(
            "rice/new/hypr/hyprland.conf",
            "# a rice
$hypr = ~/.config/hypr
$scripts = RICE/new/hypr/scripts
exec-once = : > MARK/entry-exec-once
exec=: > MARK/entry-exec
   execr-once   =   : > MARK/entry-execr-once # inline comment
exec-shutdown = : > MARK/entry-exec-shutdown
EXEC-ONCE = : > MARK/entry-upper
plugin = /nonexistent/plugin.so
source = ./sub/relative.conf
source = $hypr/conf.d/*.conf
source = RICE/new/hypr/abs.conf
source = ~/.config/caelestia/foreign.conf
bind = SUPER, Q, exec, : > MARK/bind
workspace = 1, on-created-empty: : > MARK/workspace
general {
    border_size = 2
}
",
        );
        fx.file(
            "rice/new/hypr/sub/relative.conf",
            "execr = : > MARK/relative\n$fromsub = 1\n",
        );
        fx.file(
            "rice/new/hypr/conf.d/a.conf",
            "exec-once = : > MARK/glob-a\n",
        );
        fx.file("rice/new/hypr/conf.d/b.conf", "exec = : > MARK/glob-b\n");
        fx.file(
            "rice/new/hypr/conf.d/.hidden.conf",
            "exec = : > MARK/hidden\n",
        );
        fx.file("rice/new/hypr/conf.d/c.conf.bak", "exec = : > MARK/bak\n");
        fx.file("rice/new/hypr/abs.conf", "exec-once=: > MARK/abs\n");
        fx.file(
            "home/.config/caelestia/foreign.conf",
            "\texec-once\t=\t: > MARK/foreign\n",
        );
        fx.file("rice/old/hypr/hyprland.conf", "exec-once = : > MARK/OLD\n");
        fx.file(
            "rice/old/hypr/conf.d/a.conf",
            "exec-once = : > MARK/OLD-glob\n",
        );
        mutate::create_symlink(&fx.p("home/.config/hypr"), &fx.p("rice/old/hypr")).unwrap();
    }

    /// The control: the stand-in, run directly on the *original* config,
    /// does run its exec lines. Without this, the test below could pass
    /// because the stand-in never runs anything at all.
    #[test]
    fn the_stand_in_runs_exec_lines_when_given_the_original() {
        let fx = Fx::new("control");
        nasty(&fx);
        let fake = fx.fake();
        let st = std::process::Command::new(&fake)
            .arg("--verify-config")
            .arg("-c")
            .arg(fx.p("rice/new/hypr/hyprland.conf"))
            .env_clear()
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success());
        let m = fx.markers();
        for expected in ["entry-exec-once", "entry-exec", "entry-upper", "abs"] {
            assert!(m.contains(&expected.to_string()), "{expected} not in {m:?}");
        }
    }

    /// The sandbox: the stand-in parses the copy, and every copied file,
    /// twice — and not one exec line runs.
    #[test]
    fn no_exec_line_survives_into_what_hyprland_is_given() {
        let fx = Fx::new("stripped");
        nasty(&fx);
        use_fake_hyprland(Some(fx.fake()));

        let outcome = fx.check().unwrap();
        let Outcome::Parsed {
            config,
            scratch,
            files,
            stripped,
        } = outcome
        else {
            panic!("expected a parse, got {outcome:?}");
        };

        assert_eq!(fx.markers(), Vec::<String>::new(), "an exec line ran");
        assert_eq!(config, fx.p("rice/new/hypr/hyprland.conf"));
        assert_eq!(scratch, fx.state().join("verify/ID"));
        // entry, relative, a, b, abs, foreign — not .hidden, not .bak.
        assert_eq!(files, 6);
        // 6 in the entry (5 exec + plugin), one in each of the other five.
        assert_eq!(stripped, 11);

        let mirror = scratch.join("root");
        let entry = mirror.join(
            fx.p("home/.config/hypr/hyprland.conf")
                .strip_prefix("/")
                .unwrap(),
        );
        assert_eq!(
            fx.lines("fake/argv"),
            vec![
                "--verify-config".to_string(),
                "-c".into(),
                entry.display().to_string()
            ]
        );

        // The stand-in read only copies, all six, twice — never the old
        // profile, never a file outside the scratch directory.
        let parsed = fx.lines("fake/parsed");
        assert_eq!(parsed.len(), 12, "{parsed:#?}");
        assert!(
            parsed.iter().all(|p| Path::new(p).starts_with(&mirror)),
            "{parsed:#?}"
        );

        // Every copy, read back independently of the post-check.
        for p in &parsed {
            let text = read::slurp(Path::new(p)).unwrap();
            for line in text.lines() {
                assert!(
                    !line.trim_start().to_ascii_lowercase().starts_with("exec"),
                    "{p}: {line}"
                );
                assert!(!line.contains("plugin"), "{p}: {line}");
            }
        }
        let entry_text = read::slurp(&entry).unwrap();
        let m = mirror.display().to_string();
        assert!(entry_text.contains(&format!(
            "$hypr = {m}{}",
            fx.home().join(".config/hypr").display()
        )));
        assert!(entry_text.contains(&format!(
            "$scripts = {m}{}",
            fx.home().join(".config/hypr/scripts").display()
        )));
        assert!(entry_text.contains(&format!(
            "source = {m}{}",
            fx.home().join(".config/hypr/conf.d/*.conf").display()
        )));
        // The runtime-only exec in a bind and a workspace rule are left: the
        // stand-in, like verify-config, never presses a key or makes a
        // workspace, and the marker test above proves they did not fire.
        assert!(entry_text.contains("bind = SUPER, Q, exec,"));
        // Line numbers are the original's.
        assert_eq!(
            entry_text.lines().count(),
            fx.lines("rice/new/hypr/hyprland.conf").len()
        );
    }

    /// The child's environment is the scratch copy's and nothing else: no
    /// instance signature, no display, no bus, no PATH.
    #[test]
    fn the_child_sees_only_the_scratch_copy() {
        let fx = Fx::new("env");
        nasty(&fx);
        use_fake_hyprland(Some(fx.fake()));
        let Some(Outcome::Parsed { scratch, .. }) = fx.check() else {
            panic!("expected a parse");
        };

        let mut env = std::collections::BTreeMap::new();
        for line in fx.lines("fake/env") {
            // bash: `declare -x K="v"`; dash: `export K='v'`.
            let rest = line
                .strip_prefix("declare -x ")
                .or_else(|| line.strip_prefix("export "))
                .unwrap_or(&line);
            let (k, v) = rest.split_once('=').unwrap_or((rest, ""));
            env.insert(k.to_string(), v.trim_matches(['"', '\'']).to_string());
        }
        let home = scratch
            .join("root")
            .join(fx.home().strip_prefix("/").unwrap());
        assert_eq!(env.get("HOME"), Some(&home.display().to_string()));
        assert_eq!(
            env.get("XDG_RUNTIME_DIR"),
            Some(&scratch.join("run").display().to_string())
        );
        assert_eq!(
            env.get("XDG_CONFIG_HOME"),
            Some(&home.join(".config").display().to_string())
        );
        // What the shell adds of its own accord is allowed; nothing else is.
        let allowed = [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_RUNTIME_DIR",
            "LC_ALL",
            "PWD",
            "OLDPWD",
            "SHLVL",
            "_",
        ];
        for k in env.keys() {
            assert!(allowed.contains(&k.as_str()), "the child was given {k}");
        }
        assert!(!env.contains_key("HYPRLAND_INSTANCE_SIGNATURE"));
    }

    /// A non-zero exit declines, and what the stand-in said about the copy
    /// is reported about the user's file.
    #[test]
    fn a_failed_parse_is_reported_against_the_original_file() {
        let fx = Fx::new("fails");
        nasty(&fx);
        use_fake_hyprland(Some(fx.fake()));
        fx.file("fake/verdict-fail", "");
        let Some(Outcome::Failed {
            config,
            scratch,
            detail,
        }) = fx.check()
        else {
            panic!("expected a failure");
        };
        assert_eq!(config, fx.p("rice/new/hypr/hyprland.conf"));
        assert_eq!(scratch, Some(fx.state().join("verify/ID")));
        assert_eq!(
            detail,
            format!(
                "Hyprland reported:\n      Config error in file {} at line 2: the stand-in was \
                 told to fail",
                fx.p("rice/new/hypr/hyprland.conf").display()
            )
        );
        assert_eq!(fx.markers(), Vec::<String>::new());
    }

    /// A second run with the same id gets a scratch directory of its own.
    #[test]
    fn a_taken_scratch_name_gets_a_suffix() {
        let fx = Fx::new("suffix");
        nasty(&fx);
        use_fake_hyprland(Some(fx.fake()));
        let a = fx.check();
        let b = fx.check();
        let scratch = |o: Option<Outcome>| match o {
            Some(Outcome::Parsed { scratch, .. }) => scratch,
            other => panic!("{other:?}"),
        };
        assert_eq!(scratch(a), fx.state().join("verify/ID"));
        assert_eq!(scratch(b), fx.state().join("verify/ID-1"));
    }

    /// No Hyprland: nothing is checked, nothing is written.
    #[test]
    fn without_hyprland_nothing_is_checked_or_written() {
        let fx = Fx::new("absent");
        nasty(&fx);
        use_fake_hyprland(None);
        assert_eq!(
            fx.check(),
            Some(Outcome::NotChecked {
                config: fx.p("rice/new/hypr/hyprland.conf"),
                why: NotChecked::NoHyprland,
            })
        );
        assert!(read::lstat_or_absent(&fx.state().join("verify"))
            .unwrap()
            .is_none());
    }

    /// `hyprland.lua` wins over `hyprland.conf`, as it does in Hyprland 0.55,
    /// and is never run.
    #[test]
    fn a_lua_config_is_never_run() {
        let fx = Fx::new("lua");
        nasty(&fx);
        fx.file("rice/new/hypr/hyprland.lua", "os.execute(': > MARK/lua')\n");
        use_fake_hyprland(Some(fx.fake()));
        assert_eq!(
            fx.check(),
            Some(Outcome::NotChecked {
                config: fx.p("rice/new/hypr/hyprland.lua"),
                why: NotChecked::Lua,
            })
        );
        assert!(fx.lines("fake/argv").is_empty(), "the stand-in was run");
        assert!(read::lstat_or_absent(&fx.state().join("verify"))
            .unwrap()
            .is_none());
    }

    /// A config that cannot be copied faithfully declines without Hyprland
    /// being run and without anything being written.
    #[test]
    fn an_unsandboxable_config_declines_before_anything_runs() {
        let fx = Fx::new("unsandboxable");
        fx.file(
            "rice/new/hypr/hyprland.conf",
            "exec-once = : > MARK/x\nsource = ../elsewhere.conf\n",
        );
        use_fake_hyprland(Some(fx.fake()));
        let Some(Outcome::Failed {
            scratch, detail, ..
        }) = fx.check()
        else {
            panic!("expected a refusal");
        };
        assert_eq!(scratch, None);
        assert!(detail.contains("Hyprland was not run"), "{detail}");
        assert!(detail.contains("`..`"), "{detail}");
        // Named as the file the user would edit, not as its post-switch path.
        assert!(
            detail.contains(&format!(
                "{} line 2",
                fx.p("rice/new/hypr/hyprland.conf").display()
            )),
            "{detail}"
        );
        assert!(fx.lines("fake/argv").is_empty(), "the stand-in was run");
        assert!(read::lstat_or_absent(&fx.state().join("verify"))
            .unwrap()
            .is_none());
    }

    /// A continued line in a *sourced* file — here one outside the profile,
    /// which the sandbox copies too — refuses the whole config: `ex\` joined
    /// to `ec-once = …` is an `exec-once` line no physical line spells (D67).
    /// The refusal names the file the user would edit, its line, and the
    /// rule; nothing is written and the stand-in is never run.
    #[test]
    fn a_continued_line_in_a_sourced_file_declines_before_anything_runs() {
        let fx = Fx::new("continued");
        fx.file(
            "rice/new/hypr/hyprland.conf",
            "general {\n}\nsource = ~/.config/caelestia/foreign.conf\n",
        );
        fx.file(
            "home/.config/caelestia/foreign.conf",
            "decoration {\n}\nex\\\nec-once = : > MARK/joined\n",
        );
        use_fake_hyprland(Some(fx.fake()));
        let Some(Outcome::Failed {
            scratch, detail, ..
        }) = fx.check()
        else {
            panic!("a config with a continued line was sandboxed");
        };
        assert_eq!(scratch, None);
        assert!(
            detail.contains(&format!(
                "{} line 3: the line ends in `\\`",
                fx.p("home/.config/caelestia/foreign.conf").display()
            )),
            "{detail}"
        );
        assert!(detail.contains("(D67)"), "{detail}");
        assert!(detail.contains("Hyprland was not run"), "{detail}");
        assert!(fx.lines("fake/argv").is_empty(), "the stand-in was run");
        assert!(fx.markers().is_empty(), "{:?}", fx.markers());
        assert!(read::lstat_or_absent(&fx.state().join("verify"))
            .unwrap()
            .is_none());
    }

    /// The post-check does not trust `sanitize` to have refused continued
    /// lines: a copy on disk with one is refused as a bug.
    #[test]
    fn the_post_check_refuses_a_continued_line_on_disk() {
        let fx = Fx::new("postcheck");
        let copy = fx.file("scratch/root/x.conf", "ex\\\nec = : > MARK/x\n");
        let ctx = Context {
            home: fx.home(),
            mirror: fx.p("scratch/root"),
            links: vec![],
            absent: vec![],
        };
        let err = post_check(
            &ctx,
            &[Copied {
                real: fx.p("x.conf"),
                copy,
                stripped: vec![],
            }],
        )
        .unwrap_err();
        assert!(err.to_string().contains("line 1 ends in `\\`"), "{err}");
    }

    /// Things the copier refuses: a cycle, a symlinked config, a glob that
    /// matches a directory.
    #[test]
    fn the_copier_refuses_what_it_cannot_copy_faithfully() {
        let fx = Fx::new("copier");
        use_fake_hyprland(Some(fx.fake()));

        fx.file("rice/new/hypr/hyprland.conf", "source = ./loop.conf\n");
        fx.file("rice/new/hypr/loop.conf", "source = ./hyprland.conf\n");
        let Some(Outcome::Failed { detail, .. }) = fx.check() else {
            panic!("a cycle was sandboxed");
        };
        assert!(detail.contains("sources itself"), "{detail}");

        fx.file("rice/new/hypr/hyprland.conf", "source = ./linked.conf\n");
        mutate::create_symlink(&fx.p("rice/new/hypr/linked.conf"), &fx.p("markers")).unwrap();
        let Some(Outcome::Failed { detail, .. }) = fx.check() else {
            panic!("a symlinked config was sandboxed");
        };
        assert!(detail.contains("not a regular file"), "{detail}");

        fx.file("rice/new/hypr/hyprland.conf", "source = ./d/*\n");
        mutate::make_dirs(&fx.p("rice/new/hypr/d/sub")).unwrap();
        let Some(Outcome::Failed { detail, .. }) = fx.check() else {
            panic!("a glob matching a directory was sandboxed");
        };
        assert!(detail.contains("not a regular file"), "{detail}");

        assert!(fx.lines("fake/argv").is_empty(), "the stand-in was run");
    }

    /// The scratch copy is never made on `/tmp` (a different device here, and
    /// in any case not where ricepilot keeps anything). Refused before a
    /// single file is read.
    #[test]
    fn a_scratch_directory_on_tmp_is_refused() {
        let fx = Fx::new("tmp");
        nasty(&fx);
        let err = SandboxedConfig::build(&Request {
            home: &fx.home(),
            entry: &fx.p("home/.config/hypr/hyprland.conf"),
            links: &[(fx.p("home/.config/hypr"), fx.p("rice/new/hypr"))],
            absent: &[],
            parent: Path::new("/tmp/ricepilot-verify"),
            id: "ID",
        })
        .unwrap_err();
        assert!(matches!(err, Error::Refused { rule: "R3", .. }), "{err}");
    }
}
