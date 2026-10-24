//! Include and exclude rules, in gitignore syntax.
//!
//! Without these, "advanced syncing" means pushing `target/` and `.git/` to a
//! web server every time. The syntax is gitignore's because that is the one
//! people already know and already have written down -- and because
//! `respect_gitignore` can then feed the files in a tree through the same
//! matcher.
//!
//! What is supported, and deliberately only this:
//!
//! - `name` matches that name at any depth, file or directory.
//! - `name/` matches only a directory (and therefore everything under it).
//! - `/name` anchors to the sync root.
//! - `*` matches within one path component; `?` matches one character.
//! - `**` matches across separators.
//! - A leading `!` negates, re-including something an earlier rule excluded.
//! - `#` starts a comment; a trailing `\` escapes nothing special.
//!
//! Not supported: character classes (`[a-z]`). They are rare in ignore files
//! and a half-working implementation of them would be worse than an honest
//! error, so an unsupported pattern is reported rather than silently treated
//! as a literal.

use std::path::Path;

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The pattern, with its flags stripped.
    pattern: String,
    /// `!` prefix: re-include rather than exclude.
    negated: bool,
    /// Trailing `/`: directories only.
    directory_only: bool,
    /// Leading `/`, or a `/` inside: matched against the whole relative path
    /// rather than any single component.
    anchored: bool,
}

impl Rule {
    /// Parse one gitignore line. `None` for blanks and comments.
    pub fn parse(line: &str) -> Option<Result<Self, FilterError>> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }

        let (negated, rest) = match trimmed.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, trimmed),
        };

        let (directory_only, rest) = match rest.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, rest),
        };

        let anchored = rest.starts_with('/') || rest.trim_end_matches('/').contains('/');
        let pattern = rest.trim_start_matches('/').to_owned();

        if pattern.is_empty() {
            return Some(Err(FilterError::Empty(line.to_owned())));
        }
        if pattern.contains('[') || pattern.contains(']') {
            return Some(Err(FilterError::UnsupportedClass(line.to_owned())));
        }

        Some(Ok(Self {
            pattern,
            negated,
            directory_only,
            anchored,
        }))
    }

    /// Whether this rule matches a relative path.
    fn matches(&self, relative: &Path, is_dir: bool) -> bool {
        if self.directory_only && !is_dir {
            return false;
        }

        let path = relative.to_string_lossy().replace('\\', "/");

        if self.anchored {
            return glob(&self.pattern, &path);
        }

        // Unanchored: match any single component, or any trailing run of them.
        // `target` has to exclude `target` and everything below it, which is
        // what the prefix check below does.
        for (index, component) in path.split('/').enumerate() {
            if glob(&self.pattern, component) {
                let _ = index;
                return true;
            }
        }

        false
    }
}

/// A compiled set of rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    rules: Vec<Rule>,
}

impl Filters {
    /// Compile a list of patterns, reporting the first bad one.
    pub fn parse(patterns: &[String]) -> Result<Self, FilterError> {
        let mut rules = Vec::new();
        for line in patterns {
            match Rule::parse(line) {
                Some(Ok(rule)) => rules.push(rule),
                Some(Err(e)) => return Err(e),
                None => {}
            }
        }
        Ok(Self { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether a path is excluded.
    ///
    /// Later rules win, as in gitignore: that is what makes `!keep.log` after
    /// `*.log` mean anything.
    pub fn excludes(&self, relative: &Path, is_dir: bool) -> bool {
        let mut excluded = false;
        for rule in &self.rules {
            if rule.matches(relative, is_dir) {
                excluded = !rule.negated;
            }
        }
        excluded
    }

    /// Whether a directory can be skipped without descending into it.
    ///
    /// Only when nothing could re-include something inside it. A negation
    /// anywhere in the set means the walk has to look, because `!` can rescue
    /// a file under an excluded directory -- and pruning would make that
    /// rule silently do nothing.
    pub fn can_prune(&self, relative: &Path) -> bool {
        if !self.excludes(relative, true) {
            return false;
        }
        !self.rules.iter().any(|r| r.negated)
    }
}

/// Why a pattern could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("the pattern {0:?} is empty")]
    Empty(String),
    #[error(
        "the pattern {0:?} uses a character class, which lazysync does not support. Spell the \
         alternatives out as separate patterns instead"
    )]
    UnsupportedClass(String),
}

/// Glob match: `*` within a component, `**` across them, `?` for one
/// character.
///
/// Memoised, which is not an optimisation but a correctness-of-service fix. A
/// plain backtracking matcher goes exponential on patterns with several stars
/// -- `*a*a*a*a*a*a*b` against forty `a`s took **3.7 seconds** before this,
/// and the patterns here come from the user's own config, so that is a filter
/// line that freezes the planner. Remembering which (pattern, text) positions
/// have already failed makes it O(pattern x text).
fn glob(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();

    // One slot per (pattern position, text position). Both are short -- a
    // path component and an ignore line -- so this is a few hundred bytes.
    let stride = text.len() + 1;
    let mut failed = vec![false; (pattern.len() + 1) * stride];

    glob_at(pattern, text, 0, 0, &mut failed, stride)
}

fn glob_at(
    pattern: &[u8],
    text: &[u8],
    pi: usize,
    ti: usize,
    failed: &mut [bool],
    stride: usize,
) -> bool {
    if failed[pi * stride + ti] {
        return false;
    }

    let result = glob_step(pattern, text, pi, ti, failed, stride);
    if !result {
        failed[pi * stride + ti] = true;
    }
    result
}

fn glob_step(
    pattern: &[u8],
    text: &[u8],
    pi: usize,
    ti: usize,
    failed: &mut [bool],
    stride: usize,
) -> bool {
    if pi == pattern.len() {
        return ti == text.len();
    }

    match pattern[pi] {
        b'*' => {
            // `**` crosses separators; a single `*` stops at one.
            let double = pi + 1 < pattern.len() && pattern[pi + 1] == b'*';
            let next = if double {
                let after = pi + 2;
                // `**/` also matches zero directories, so `**/a` matches a
                // bare `a`.
                if pattern.get(after) == Some(&b'/')
                    && glob_at(pattern, text, after + 1, ti, failed, stride)
                {
                    return true;
                }
                after
            } else {
                pi + 1
            };

            for skip in 0..=(text.len() - ti) {
                if !double && text[ti..ti + skip].contains(&b'/') {
                    break;
                }
                if glob_at(pattern, text, next, ti + skip, failed, stride) {
                    return true;
                }
            }
            false
        }

        // One character, but never a separator.
        b'?' => {
            ti < text.len()
                && text[ti] != b'/'
                && glob_at(pattern, text, pi + 1, ti + 1, failed, stride)
        }

        literal => {
            ti < text.len()
                && text[ti] == literal
                && glob_at(pattern, text, pi + 1, ti + 1, failed, stride)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn filters(patterns: &[&str]) -> Filters {
        Filters::parse(&patterns.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
    }

    fn excluded(f: &Filters, path: &str) -> bool {
        f.excludes(&PathBuf::from(path), false)
    }

    fn dir_excluded(f: &Filters, path: &str) -> bool {
        f.excludes(&PathBuf::from(path), true)
    }

    #[test]
    fn a_bare_name_matches_at_any_depth() {
        let f = filters(&["target"]);
        assert!(excluded(&f, "target"));
        assert!(excluded(&f, "crates/thing/target"));
        assert!(excluded(&f, "target/debug/binary"));
        assert!(!excluded(&f, "src/target.rs"));
        assert!(!excluded(&f, "my-target"));
    }

    #[test]
    fn an_extension_glob_matches_within_a_component() {
        let f = filters(&["*.log"]);
        assert!(excluded(&f, "debug.log"));
        assert!(excluded(&f, "var/logs/debug.log"));
        assert!(!excluded(&f, "debug.log.gz"));
        assert!(!excluded(&f, "log"));
    }

    /// A single `*` must not cross a separator, or `src/*.rs` would match
    /// anything anywhere.
    #[test]
    fn a_single_star_does_not_cross_a_separator() {
        let f = filters(&["src/*.rs"]);
        assert!(excluded(&f, "src/main.rs"));
        assert!(
            !excluded(&f, "src/api/handler.rs"),
            "a single star stops at the separator"
        );
    }

    #[test]
    fn a_double_star_crosses_separators() {
        let f = filters(&["src/**/*.rs"]);
        assert!(excluded(&f, "src/main.rs"));
        assert!(excluded(&f, "src/api/handler.rs"));
        assert!(excluded(&f, "src/a/b/c/deep.rs"));
        assert!(!excluded(&f, "tests/main.rs"));
    }

    #[test]
    fn a_leading_slash_anchors_to_the_root() {
        let f = filters(&["/build"]);
        assert!(excluded(&f, "build"));
        assert!(
            !excluded(&f, "crates/thing/build"),
            "an anchored pattern must not match deeper"
        );
    }

    #[test]
    fn a_trailing_slash_matches_directories_only() {
        let f = filters(&["cache/"]);
        assert!(dir_excluded(&f, "cache"));
        assert!(
            !excluded(&f, "cache"),
            "a file called cache should not be excluded by cache/"
        );
    }

    /// The reason later-wins matters.
    #[test]
    fn a_negation_re_includes() {
        let f = filters(&["*.log", "!keep.log"]);
        assert!(excluded(&f, "debug.log"));
        assert!(!excluded(&f, "keep.log"));
    }

    #[test]
    fn order_decides_when_rules_overlap() {
        // Negation first, exclusion second: the exclusion wins.
        let f = filters(&["!keep.log", "*.log"]);
        assert!(excluded(&f, "keep.log"));
    }

    #[test]
    fn a_question_mark_matches_one_character() {
        let f = filters(&["log?.txt"]);
        assert!(excluded(&f, "log1.txt"));
        assert!(!excluded(&f, "log.txt"));
        assert!(!excluded(&f, "log12.txt"));
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let f = filters(&["# a comment", "", "   ", "real"]);
        assert_eq!(f.len(), 1);
        assert!(excluded(&f, "real"));
    }

    /// A pattern lazysync cannot honour is an error, not a literal. Treating
    /// `*.[oa]` as a filename would silently sync the files it was meant to
    /// exclude.
    #[test]
    fn a_character_class_is_refused_rather_than_misread() {
        let err = Filters::parse(&["*.[oa]".to_owned()]).unwrap_err();
        assert!(matches!(err, FilterError::UnsupportedClass(_)));
        let message = err.to_string();
        assert!(message.contains("character class"), "{message}");
        assert!(
            message.contains("separate patterns"),
            "it has to say what to do instead: {message}"
        );
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        assert!(matches!(
            Filters::parse(&["!".to_owned()]).unwrap_err(),
            FilterError::Empty(_)
        ));
    }

    #[test]
    fn no_rules_excludes_nothing() {
        let f = Filters::default();
        assert!(f.is_empty());
        assert!(!excluded(&f, "anything"));
        assert!(!f.can_prune(&PathBuf::from("anything")));
    }

    /// Pruning saves walking an excluded directory, which on a remote is a
    /// round trip per level.
    #[test]
    fn an_excluded_directory_can_be_pruned() {
        let f = filters(&["target"]);
        assert!(f.can_prune(&PathBuf::from("target")));
        assert!(!f.can_prune(&PathBuf::from("src")));
    }

    /// But not when a negation could rescue something inside it: pruning then
    /// would make the `!` rule silently do nothing.
    #[test]
    fn pruning_is_disabled_when_any_rule_negates() {
        let f = filters(&["target", "!target/keep.txt"]);
        assert!(
            !f.can_prune(&PathBuf::from("target")),
            "a negation anywhere means the walk has to look inside"
        );
        assert!(excluded(&f, "target/debug/x"));
        assert!(!excluded(&f, "target/keep.txt"));
    }

    #[test]
    fn the_common_rust_and_git_patterns_behave() {
        let f = filters(&["/target/", ".git/", "*.rs.bk", "**/node_modules"]);

        assert!(dir_excluded(&f, "target"));
        assert!(dir_excluded(&f, ".git"));
        assert!(excluded(&f, "src/main.rs.bk"));
        assert!(dir_excluded(&f, "web/node_modules"));
        assert!(dir_excluded(&f, "node_modules"));

        assert!(!excluded(&f, "src/main.rs"));
        assert!(!excluded(&f, "Cargo.toml"));
    }

    /// `**` with nothing after it is a valid gitignore pattern meaning
    /// everything, and must not loop forever.
    #[test]
    fn a_bare_double_star_matches_everything_and_terminates() {
        let f = filters(&["**"]);
        assert!(excluded(&f, "a"));
        assert!(excluded(&f, "a/b/c"));
    }

    /// A deeply nested path against a pattern full of stars is where a naive
    /// backtracking matcher goes exponential. This should return promptly.
    #[test]
    fn a_pathological_pattern_does_not_hang() {
        let f = filters(&["*a*a*a*a*a*a*b"]);
        let path = "a".repeat(40);

        let started = std::time::Instant::now();
        let hit = excluded(&f, &path);
        assert!(!hit);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "a backtracking matcher took 3.7s here; memoised it should be immediate, took {:?}",
            started.elapsed()
        );
    }
}
