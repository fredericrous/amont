//! Which packages a workspace root's own script already runs: where the root
//! says its members are.
//!
//! A repository declares that a root script runs every member
//! (`set rootRunsMembers test`), and the push then skips that script in each
//! changed member the root covers. "Covers" is a question about the workspace
//! globs, answered here from the files a package manager itself reads:
//! `pnpm-workspace.yaml` for a pnpm root (and only that file), else the root
//! `package.json`'s `workspaces`, as an array or as `{"packages": [...]}`.
//!
//! The parsers are hand-written and std-only, because this crate has no
//! dependencies, and they read the plain shapes people write: quoted or plain
//! strings, `!` negations, `*`, `**` and `?`. Anything else (a character class,
//! a brace set, an anchor, an escape, a file that is not what it should be)
//! makes the answer `None`, and a member the answer does not cover runs: the
//! safe side, where the worst outcome is a suite that ran twice.

use super::json_scan::{string_end, top_level_value};
use std::path::Path;

/// Where a workspace's members are: the include globs, and the `!` ones that
/// take directories back out. Each glob is its `/`-separated segments.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Members {
    includes: Vec<Vec<String>>,
    excludes: Vec<Vec<String>>,
}

/// The members of the workspace rooted at `root`, or `None` when it declares
/// none or declares them in a form this cannot read.
pub(crate) fn members_of(root: &Path) -> Option<Members> {
    let yaml = root.join("pnpm-workspace.yaml");
    if yaml.is_file() {
        // A pnpm root reads this file and no other: its `package.json`
        // `workspaces` would be ignored by pnpm, so it must not count here.
        return Members::from_pnpm_yaml(&std::fs::read_to_string(yaml).ok()?);
    }
    Members::from_package_json(&std::fs::read_to_string(root.join("package.json")).ok()?)
}

impl Members {
    /// Is the package directory `dir` (relative to the root, `/`-separated) a
    /// member? A path that could not be what git said (a newline, the
    /// replacement character a lossy conversion leaves) is not covered, and
    /// neither is the root itself.
    pub(crate) fn covers(&self, dir: &str) -> bool {
        if dir.is_empty() || dir.contains(['\n', '\r', '\u{fffd}']) {
            return false;
        }
        let segments: Vec<&str> = dir.split('/').collect();
        self.includes
            .iter()
            .any(|glob| matches_path(glob, &segments))
            && !self
                .excludes
                .iter()
                .any(|glob| matches_path(glob, &segments))
    }

    fn from_patterns(patterns: Vec<String>) -> Option<Members> {
        let (mut includes, mut excludes) = (Vec::new(), Vec::new());
        for pattern in patterns {
            let (negated, glob) = match pattern.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, pattern.as_str()),
            };
            let glob = glob
                .strip_prefix("./")
                .unwrap_or(glob)
                .trim_end_matches('/');
            // Character classes, brace sets, groups and escapes are glob
            // features this does not implement: unreadable, not guessed at.
            if glob.is_empty() || glob.contains(['[', ']', '{', '}', '(', ')', '\\']) {
                return None;
            }
            let segments: Vec<String> = glob.split('/').map(str::to_string).collect();
            if segments.iter().any(String::is_empty) {
                return None;
            }
            if negated {
                excludes.push(segments);
            } else {
                includes.push(segments);
            }
        }
        (!includes.is_empty()).then_some(Members { includes, excludes })
    }

    fn from_package_json(text: &str) -> Option<Members> {
        let workspaces = top_level_value(text, "workspaces")?;
        let list = if workspaces.starts_with('[') {
            workspaces
        } else {
            top_level_value(workspaces, "packages")?
        };
        Members::from_patterns(string_items(list)?)
    }

    fn from_pnpm_yaml(text: &str) -> Option<Members> {
        // Anchors, aliases and merge keys are YAML this does not follow.
        if text.contains(['&', '*']) && text.lines().any(is_yaml_reference) {
            return None;
        }
        let lines: Vec<&str> = text.lines().map(strip_yaml_comment).collect();
        let mut found = None;
        let mut at = 0;
        while at < lines.len() {
            let line = lines[at];
            at += 1;
            let Some(rest) = line.strip_prefix("packages:") else {
                continue;
            };
            if found.is_some() {
                return None; // two `packages` keys: not a file to guess about
            }
            let rest = rest.trim();
            if rest.starts_with('[') {
                found = Some(yaml_flow_items(rest)?);
                continue;
            }
            if !rest.is_empty() {
                return None;
            }
            let mut items = Vec::new();
            while at < lines.len() {
                let trimmed = lines[at].trim();
                if trimmed.is_empty() {
                    at += 1;
                    continue;
                }
                let Some(item) = trimmed.strip_prefix('-') else {
                    break; // the next key, left for the outer loop to read
                };
                items.push(yaml_scalar(item.trim())?);
                at += 1;
            }
            found = Some(items);
        }
        Members::from_patterns(found?)
    }
}

/// A YAML line that uses an anchor or an alias (`&name`, `*name`) outside a
/// quoted scalar. A bare `*` in a glob (`- packages/*`) is not one.
fn is_yaml_reference(line: &str) -> bool {
    let trimmed = line.trim_start().trim_start_matches('-').trim_start();
    let alias = trimmed
        .strip_prefix('*')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
    trimmed.starts_with('&') || alias || trimmed.starts_with("<<")
}

/// `line` without a trailing ` # comment`, quotes respected.
fn strip_yaml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return line[..i].trim_end(),
            None => {}
        }
        previous = c;
    }
    line.trim_end()
}

/// One YAML scalar: quoted with `'` or `"`, or plain. A plain scalar that
/// starts with a YAML indicator is a tag, an alias or a mapping, not a glob.
fn yaml_scalar(text: &str) -> Option<String> {
    let text = text.trim();
    for quote in ['\'', '"'] {
        if let Some(inner) = text.strip_prefix(quote) {
            return inner.strip_suffix(quote).map(str::to_string);
        }
    }
    if text.is_empty() || text.starts_with(['!', '&', '*', '|', '>', '?', ':', '{', '[']) {
        return None;
    }
    if text.contains(": ") {
        return None;
    }
    Some(text.to_string())
}

/// The items of a one-line YAML list, `['a', 'b']`.
fn yaml_flow_items(text: &str) -> Option<Vec<String>> {
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    inner
        .split(',')
        .filter(|item| !item.trim().is_empty())
        .map(yaml_scalar)
        .collect()
}

/// `*` and `?` within one segment; nothing else is special.
fn segment_matches(glob: &str, text: &str) -> bool {
    let (glob, text): (Vec<char>, Vec<char>) = (glob.chars().collect(), text.chars().collect());
    let (mut g, mut t) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while t < text.len() {
        if g < glob.len() && (glob[g] == '?' || glob[g] == text[t]) {
            g += 1;
            t += 1;
        } else if g < glob.len() && glob[g] == '*' {
            star = Some(g);
            resume = t;
            g += 1;
        } else if let Some(s) = star {
            g = s + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    glob[g..].iter().all(|c| *c == '*')
}

/// A whole glob against a whole path: `**` matches zero or more segments.
fn matches_path(glob: &[String], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| matches_path(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((segment, remaining)) => {
                segment_matches(first, segment) && matches_path(rest, remaining)
            }
            None => false,
        },
    }
}

/// The strings of a JSON array of plain strings, or `None` for anything else:
/// another kind of item, or a string with an escape in it.
fn string_items(array: &str) -> Option<Vec<String>> {
    let inner = array.strip_prefix('[')?.strip_suffix(']')?;
    let bytes = inner.as_bytes();
    let (mut items, mut i) = (Vec::new(), 0usize);
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return Some(items);
        }
        if bytes[i] != b'"' {
            return None;
        }
        let close = string_end(bytes, i + 1)?;
        let content = &inner[i + 1..close];
        if content.contains('\\') {
            return None;
        }
        items.push(content.to_string());
        i = close + 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => return Some(items),
            Some(b',') => i += 1,
            Some(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Option<Members> {
        Members::from_package_json(text)
    }

    fn yaml(text: &str) -> Option<Members> {
        Members::from_pnpm_yaml(text)
    }

    #[test]
    fn a_workspaces_array_names_the_members() {
        let members = json(r#"{"name":"r","workspaces":["packages/*","apps/web"]}"#).unwrap();
        assert!(members.covers("packages/a") && members.covers("apps/web"));
        assert!(!members.covers("packages/a/deep"), "`*` is one segment");
        assert!(!members.covers("apps/other") && !members.covers("tools/x"));
        assert!(!members.covers(""), "the root is not its own member");
    }

    #[test]
    fn the_object_form_reads_its_packages() {
        let members = json(r#"{"workspaces":{"packages":["libs/**"],"nohoist":["x"]}}"#).unwrap();
        assert!(members.covers("libs/a") && members.covers("libs/a/b"));
        // The tools look for `<glob>/package.json`, so `**` also takes zero
        // segments and `libs` itself is a member when it has a package.json.
        assert!(members.covers("libs"));
        assert!(!members.covers("apps/a"));
    }

    #[test]
    fn a_negation_takes_directories_back_out() {
        let members = yaml("packages:\n  - 'packages/*'\n  - '!packages/legacy'\n").unwrap();
        assert!(members.covers("packages/a") && !members.covers("packages/legacy"));
        let deep = json(r#"{"workspaces":["**","!**/fixtures/**"]}"#).unwrap();
        assert!(deep.covers("a/b") && !deep.covers("a/fixtures/x"));
    }

    #[test]
    fn a_pnpm_file_in_each_of_its_common_shapes() {
        for text in [
            "packages:\n  - 'apps/*'\n  - \"libs/*\"\n",
            "packages:\n- apps/*\n- libs/*\n",
            "packages: ['apps/*', 'libs/*']\n",
            "# the workspace\npackages:\n  - apps/*   # the apps\n  - libs/*\n\ncatalog:\n  react: ^19\n",
        ] {
            let members = yaml(text).unwrap_or_else(|| panic!("unreadable: {text}"));
            assert!(members.covers("apps/a") && members.covers("libs/b"), "{text}");
            assert!(!members.covers("tools/c"), "{text}");
        }
    }

    /// pnpm reads `pnpm-workspace.yaml` and nothing else, so a root that has one
    /// does not get members from its `package.json`.
    #[test]
    fn a_pnpm_root_reads_only_its_workspace_file() {
        let dir = std::env::temp_dir().join(format!("amont-js-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // best-effort temp cleanup; a leftover dir is harmless
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"workspaces":["pkg/*"]}"#).unwrap();
        std::fs::write(dir.join("pnpm-workspace.yaml"), "packages:\n  - 'apps/*'\n").unwrap();
        let members = members_of(&dir).unwrap();
        assert!(members.covers("apps/a"));
        assert!(
            !members.covers("pkg/a"),
            "package.json `workspaces` is not pnpm's"
        );
        std::fs::remove_file(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(members_of(&dir).unwrap().covers("pkg/a"));
        let _ = std::fs::remove_dir_all(&dir); // best-effort temp cleanup; a leftover dir is harmless
    }

    /// Whatever cannot be read answers nothing, and an uncovered member runs.
    #[test]
    fn what_cannot_be_read_covers_nothing() {
        assert!(json(r#"{"name":"r"}"#).is_none(), "no workspaces");
        assert!(json(r#"{"workspaces":"packages/*"}"#).is_none(), "a string");
        assert!(json(r#"{"workspaces":["a\\b"]}"#).is_none(), "an escape");
        assert!(json(r#"{"workspaces":[1]}"#).is_none(), "not strings");
        assert!(
            json(r#"{"workspaces":["pkg/{a,b}"]}"#).is_none(),
            "a brace set"
        );
        assert!(json(r#"{"workspaces":["pkg/[ab]"]}"#).is_none(), "a class");
        assert!(json(r#"{"workspaces":[]}"#).is_none(), "an empty list");
        assert!(json(r#"{"workspaces":["pkg/*""#).is_none(), "truncated");
        assert!(yaml("name: x\n").is_none(), "no packages key");
        assert!(yaml("packages: &p\n  - 'a/*'\n").is_none(), "an anchor");
        assert!(yaml("packages:\n  - *shared\n").is_none(), "an alias");
        assert!(yaml("packages:\n  - !!str a\n").is_none(), "a tag");
        assert!(
            yaml("packages:\n  - 'a/*'\npackages:\n  - 'b/*'\n").is_none(),
            "twice"
        );
        assert!(yaml("packages: a/*\n").is_none(), "a scalar");
    }

    #[test]
    fn a_path_that_is_not_what_git_said_is_never_covered() {
        let members = json(r#"{"workspaces":["**"]}"#).unwrap();
        assert!(members.covers("a/b"));
        assert!(!members.covers("a/b\nc"));
        assert!(!members.covers("a/\u{fffd}"));
    }

    #[test]
    fn globs_match_within_a_segment_and_across_with_double_star() {
        let members = json(r#"{"workspaces":["pkg-*","?x/lib","a/**/z"]}"#).unwrap();
        assert!(members.covers("pkg-one") && members.covers("ax/lib"));
        assert!(members.covers("a/z") && members.covers("a/b/c/z"));
        assert!(!members.covers("pkg") && !members.covers("axx/lib") && !members.covers("a/b"));
    }

    /// A name after `*` is an alias, so the file is unreadable; a bare glob is
    /// not, and a plain `- **` is deliberately refused as a glob later.
    #[test]
    fn an_alias_is_a_reference_and_a_glob_is_not() {
        assert!(is_yaml_reference("  - *-utils"));
        assert!(is_yaml_reference("  - *pkg"));
        assert!(!is_yaml_reference("  - packages/*"));
        assert!(!is_yaml_reference("  - '*'"));
    }
}
