//! Fold subdirectory sessions back into their project.
//!
//! Claude Code names a transcript folder after the working directory the session
//! started in, so running `claude` inside `Greedout\src-tauri` creates a folder
//! that looks like a project of its own. Left alone, one project shows up as two
//! (or five) unrelated entries in the list and the Explorer tree.
//!
//! Grouping needs no filesystem access and no git: the folder names alone say
//! where the projects are. A user keeps their code in two or three places, so
//! nearly every project directory shares one of a few parents. Those frequent
//! parents are workspace roots, and a project is whatever directory sits directly
//! under one. Anything deeper folds up into it.
//!
//! Two rules keep that honest:
//!
//! 1. A directory that has its own sessions is a project, never a workspace root.
//!    `Greedout` holds `src-tauri`, but sessions run in `Greedout` itself, so it
//!    stays a project and `src-tauri` folds into it.
//! 2. A parent needs at least two distinct project directories under it before it
//!    counts as a root. One child is not evidence of a container.
//!
//! Directories that match neither rule (nothing above them qualifies) fold into
//! the nearest ancestor that is itself a project, and otherwise stand alone. As
//! more siblings appear, their shared parent crosses the threshold and they group
//! themselves without any special case.
//!
//! This is a display concern only. The transcript path, session identity and the
//! cost math all keep working off the real directory.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// A parent needs this many distinct project directories to count as a workspace
/// root. Two siblings is the weakest evidence that still means "container".
const ROOT_MIN_CHILDREN: usize = 2;

/// How far up to walk before giving up. Deeper than this and the path is strange
/// enough that standing alone is the safer answer.
const MAX_WALK: usize = 12;

/// Where one session directory belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The project directory this session belongs to (its own dir, when it is
    /// already a project).
    pub root: PathBuf,
    /// Short name of `root`, for the project label.
    pub label: String,
    /// Path from `root` down to the session directory, `/`-joined, when the
    /// session ran in a subdirectory: `src-tauri`, `packages/ui`. `None` when the
    /// session ran in the project directory itself.
    pub sub: Option<String>,
}

/// Compare paths the way the platform does. Windows paths differ in case between
/// the encoded folder name and what the user typed, and that must not split one
/// project in two.
fn key(p: &Path) -> String {
    let s = p.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Last component of a path, falling back to the whole thing for a drive or root
/// (`C:\` has no file name).
fn short_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().trim_end_matches(['/', '\\']).to_string())
}

/// A parent worth considering: `Some(parent)` unless `p` is already a root or a
/// bare drive, which can never be a meaningful workspace root.
fn parent_of(p: &Path) -> Option<PathBuf> {
    let parent = p.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    // A bare drive/filesystem root (`C:\`, `/`) groups everything under it, which
    // is never the grouping the user meant.
    let only_root = parent
        .components()
        .all(|c| matches!(c, Component::Prefix(_) | Component::RootDir));
    if only_root {
        return None;
    }
    Some(parent.to_path_buf())
}

/// Orca fans a seed project out into many ephemeral clones under
/// `.../orca/workspaces/<ws>/<clone>`. To the user those clones are one project,
/// named after the workspace `<ws>`, not dozens of `quorum-fj-*` entries. They are
/// distinct sibling directories, so the generic parent-counting rules keep them
/// apart (their container looks like a workspace root); this special case folds
/// every path inside a workspace directory into that directory. Returns the `<ws>`
/// directory when `d` sits under one, else None.
fn orca_workspace_root(d: &Path) -> Option<PathBuf> {
    let comps: Vec<Component> = d.components().collect();
    let seg = |c: &Component| c.as_os_str().to_string_lossy().to_lowercase();
    for i in 0..comps.len() {
        // `orca` immediately followed by `workspaces`; the next component is `<ws>`,
        // the project directory. Require a component after `<ws>` so a session that
        // ran in `<ws>` itself is left to the normal path (it is already a project).
        if seg(&comps[i]) == "orca"
            && comps.get(i + 1).map(&seg).as_deref() == Some("workspaces")
            && comps.len() > i + 3
        {
            return Some(comps[..=i + 2].iter().collect());
        }
    }
    None
}

/// Group every scanned project directory under the project it belongs to.
///
/// `dirs` is the decoded working directory of each transcript folder; every one of
/// them has sessions, which is why it exists at all. The returned map is keyed the
/// same way (one entry per input, in the caller's own path spelling).
pub fn group_dirs(dirs: &[PathBuf]) -> HashMap<PathBuf, Group> {
    // Every input directory is a project directory by definition: sessions ran in
    // it. That is rule 1, and it also means a root can never be one of these.
    // Keyed by the comparison form, valued with the caller's own spelling, so a
    // folded child reports the project's real casing rather than its own.
    let projects: HashMap<String, PathBuf> = dirs.iter().map(|d| (key(d), d.clone())).collect();

    // Rule 2: count distinct children per parent. Only directories that are not
    // themselves projects can qualify.
    let mut children: HashMap<String, HashSet<String>> = HashMap::new();
    for d in dirs {
        if let Some(p) = parent_of(d) {
            children.entry(key(&p)).or_default().insert(key(d));
        }
    }
    let roots: HashSet<String> = children
        .into_iter()
        .filter(|(k, kids)| kids.len() >= ROOT_MIN_CHILDREN && !projects.contains_key(k))
        .map(|(k, _)| k)
        .collect();

    let mut out = HashMap::with_capacity(dirs.len());
    for d in dirs {
        // Orca workspace clones fold into their `<ws>` directory regardless of the
        // generic rules, which would otherwise treat the workspace as a container of
        // independent projects.
        let root = orca_workspace_root(d).unwrap_or_else(|| root_for(d, &projects, &roots));
        // strip_prefix is case-sensitive, and the root may be spelled differently
        // from the child, so count components instead of matching text.
        let sub = (key(d) != key(&root))
            .then(|| d.components().skip(root.components().count()).collect::<PathBuf>())
            .as_deref()
            .map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|s| !s.is_empty());
        out.insert(
            d.clone(),
            Group {
                label: short_name(&root),
                root,
                sub,
            },
        );
    }
    out
}

/// Walk up from `d` to the directory that should own it. The nearest ancestor that
/// is itself a project wins; failing that, the ancestor sitting directly under a
/// workspace root; failing both, `d` stands alone.
fn root_for(d: &Path, projects: &HashMap<String, PathBuf>, roots: &HashSet<String>) -> PathBuf {
    let mut cur = d.to_path_buf();
    for _ in 0..MAX_WALK {
        let Some(parent) = parent_of(&cur) else { break };
        let pk = key(&parent);
        // An ancestor with its own sessions is the project; nearest one wins, so
        // a package inside a repo does not skip past it to the repo.
        if let Some(canonical) = projects.get(&pk) {
            return canonical.clone();
        }
        if roots.contains(&pk) {
            return projects.get(&key(&cur)).cloned().unwrap_or(cur);
        }
        cur = parent;
    }
    d.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn group(dirs: &[&str]) -> HashMap<PathBuf, Group> {
        group_dirs(&dirs.iter().map(|s| p(s)).collect::<Vec<_>>())
    }

    #[test]
    fn subdir_folds_into_its_project() {
        let g = group(&[
            r"C:\code\Greedout",
            r"C:\code\Greedout\src-tauri",
            r"C:\projects\OtherApp",
        ]);
        let sub = &g[&p(r"C:\code\Greedout\src-tauri")];
        assert_eq!(sub.root, p(r"C:\code\Greedout"));
        assert_eq!(sub.label, "Greedout");
        assert_eq!(sub.sub.as_deref(), Some("src-tauri"));
        // The project itself is untouched.
        let own = &g[&p(r"C:\code\Greedout")];
        assert_eq!(own.label, "Greedout");
        assert_eq!(own.sub, None);
    }

    #[test]
    fn deep_subdir_keeps_the_whole_tail() {
        let g = group(&[
            r"C:\dev\app",
            r"C:\dev\app\packages\ui\src",
            r"C:\dev\other",
        ]);
        let deep = &g[&p(r"C:\dev\app\packages\ui\src")];
        assert_eq!(deep.root, p(r"C:\dev\app"));
        assert_eq!(deep.sub.as_deref(), Some("packages/ui/src"));
    }

    #[test]
    fn a_project_with_sessions_is_never_a_root() {
        // `app` holds two subdir sessions and would out-count a real root, but it
        // has sessions of its own, so both fold into it rather than splitting.
        let g = group(&[
            r"C:\dev\app",
            r"C:\dev\app\api",
            r"C:\dev\app\web",
            r"C:\dev\other",
        ]);
        assert_eq!(g[&p(r"C:\dev\app\api")].root, p(r"C:\dev\app"));
        assert_eq!(g[&p(r"C:\dev\app\web")].root, p(r"C:\dev\app"));
    }

    #[test]
    fn siblings_under_a_container_stay_separate() {
        // Nobody runs a session in `packages` itself, and it holds three projects,
        // so it is a workspace root and its children are their own projects.
        let g = group(&[
            r"C:\dev\repo\packages\ui",
            r"C:\dev\repo\packages\api",
            r"C:\dev\repo\packages\cli",
        ]);
        for d in ["ui", "api", "cli"] {
            let e = &g[&p(&format!(r"C:\dev\repo\packages\{d}"))];
            assert_eq!(e.label, d);
            assert_eq!(e.sub, None);
        }
    }

    #[test]
    fn orca_workspace_clones_fold_into_the_workspace() {
        // Many ephemeral clones under one orca workspace are one project (`<ws>`),
        // not one project each, even though they are distinct sibling directories.
        let g = group(&[
            r"C:\Users\j\orca\workspaces\sample-app\quorum-fj-0",
            r"C:\Users\j\orca\workspaces\sample-app\quorum-fj-1",
            r"C:\Users\j\orca\workspaces\sample-app\quorum-fj-1c31f481-2",
            r"C:\claude-local\quorum",
        ]);
        for clone in ["quorum-fj-0", "quorum-fj-1", "quorum-fj-1c31f481-2"] {
            let e = &g[&p(&format!(r"C:\Users\j\orca\workspaces\sample-app\{clone}"))];
            assert_eq!(e.root, p(r"C:\Users\j\orca\workspaces\sample-app"));
            assert_eq!(e.label, "sample-app");
            assert_eq!(e.sub.as_deref(), Some(clone));
        }
        // A real project that merely shares the `quorum` leaf name is untouched.
        assert_eq!(g[&p(r"C:\claude-local\quorum")].label, "quorum");
    }

    #[test]
    fn a_lone_directory_stands_alone() {
        let g = group(&[r"C:\work\thing", r"C:\code\Greedout"]);
        let e = &g[&p(r"C:\work\thing")];
        assert_eq!(e.root, p(r"C:\work\thing"));
        assert_eq!(e.sub, None);
    }

    #[test]
    fn the_drive_is_never_a_workspace_root() {
        // Two projects sitting straight on `C:\` must not make the drive a root.
        let g = group(&[r"C:\one", r"C:\two"]);
        assert_eq!(g[&p(r"C:\one")].root, p(r"C:\one"));
        assert_eq!(g[&p(r"C:\two")].root, p(r"C:\two"));
    }

    #[test]
    fn case_differences_do_not_split_a_project() {
        if !cfg!(windows) {
            return;
        }
        let g = group(&[
            r"C:\code\Greedout",
            r"C:\code\greedout\src-tauri",
            r"C:\projects\OtherApp",
        ]);
        assert_eq!(
            g[&p(r"C:\code\greedout\src-tauri")].root,
            p(r"C:\code\Greedout")
        );
    }

    #[test]
    fn posix_paths_group_the_same_way() {
        let g = group(&[
            "/home/j/code/greedout",
            "/home/j/code/greedout/src-tauri",
            "/home/j/code/player",
        ]);
        let sub = &g[&p("/home/j/code/greedout/src-tauri")];
        assert_eq!(sub.root, p("/home/j/code/greedout"));
        assert_eq!(sub.sub.as_deref(), Some("src-tauri"));
    }
}
