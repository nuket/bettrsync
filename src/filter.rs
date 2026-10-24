// bettrsync
// Copyright (c) 2026 Max Vilimpoc
// SPDX-License-Identifier: MIT

//! Exclude patterns, as rsync reads them: a pattern without a slash is
//! matched against a name anywhere in the tree, one with a slash
//! against the path under the destination, anchored there if it starts
//! with a slash and ending anywhere on a directory boundary if not; a
//! trailing slash matches directories only. `*` matches within a name,
//! `**` across slashes, `?` one character, `[a-z]` a class, `\` escapes.

struct Pattern {
    glob: Vec<u8>,
    anchored: bool,
    has_slash: bool,
    dir_only: bool,
}

#[derive(Default)]
pub struct Excludes {
    patterns: Vec<Pattern>,
}

impl Excludes {
    pub fn new(patterns: &[String]) -> Excludes {
        let patterns = patterns
            .iter()
            .filter_map(|p| {
                let mut glob = p.as_bytes().to_vec();
                let dir_only = glob.len() > 1 && glob.ends_with(b"/");
                if dir_only {
                    glob.pop();
                }
                let anchored = glob.starts_with(b"/");
                if anchored {
                    glob.remove(0);
                }
                if glob.is_empty() {
                    return None;
                }
                let has_slash = glob.contains(&b'/') || glob.windows(2).any(|w| w == b"**");
                Some(Pattern { glob, anchored, has_slash, dir_only })
            })
            .collect();
        Excludes { patterns }
    }

    /// Whether the entry at `rel` under the destination is excluded.
    pub fn matches(&self, rel: &[u8], is_dir: bool) -> bool {
        self.patterns.iter().any(|p| p.matches(rel, is_dir))
    }
}

impl Pattern {
    fn matches(&self, rel: &[u8], is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        if self.anchored {
            return glob(&self.glob, rel);
        }
        if !self.has_slash {
            let name = rel.rsplit(|b| *b == b'/').next().unwrap_or(rel);
            return glob(&self.glob, name);
        }
        // Unanchored: the pattern ends the path, at a directory boundary.
        if glob(&self.glob, rel) {
            return true;
        }
        rel.iter().enumerate().filter(|(_, b)| **b == b'/').any(|(i, _)| glob(&self.glob, &rel[i + 1..]))
    }
}

/// `*` within a name, `**` across slashes, `?`, `[...]`, `\`.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            // `**/` may also match no directory at all.
            let rest = &p[2..];
            if rest.first() == Some(&b'/') && glob(&rest[1..], s) {
                return true;
            }
            (0..=s.len()).any(|k| glob(rest, &s[k..]))
        }
        Some(b'*') => {
            let rest = &p[1..];
            let mut k = 0;
            loop {
                if glob(rest, &s[k..]) {
                    return true;
                }
                if k >= s.len() || s[k] == b'/' {
                    return false;
                }
                k += 1;
            }
        }
        Some(b'?') => !s.is_empty() && s[0] != b'/' && glob(&p[1..], &s[1..]),
        Some(b'[') => {
            let Some(c) = s.first().copied().filter(|c| *c != b'/') else { return false };
            let (hit, after) = class(&p[1..], c);
            match after {
                Some(after) => hit && glob(after, &s[1..]),
                // No closing bracket: a literal [.
                None => c == b'[' && glob(&p[1..], &s[1..]),
            }
        }
        Some(b'\\') if p.len() > 1 => !s.is_empty() && s[0] == p[1] && glob(&p[2..], &s[1..]),
        Some(c) => !s.is_empty() && s[0] == *c && glob(&p[1..], &s[1..]),
    }
}

/// Matches `c` against the class starting after `[`: whether it is in
/// it, and the pattern after the closing bracket, or None if there is
/// no closing bracket.
fn class(p: &[u8], c: u8) -> (bool, Option<&[u8]>) {
    let mut i = 0;
    let negate = matches!(p.first(), Some(b'!') | Some(b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    while i < p.len() {
        let lo = p[i];
        if lo == b']' && !first {
            return (hit != negate, Some(&p[i + 1..]));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            hit |= lo <= c && c <= p[i + 2];
            i += 3;
        } else {
            hit |= lo == c;
            i += 1;
        }
    }
    (false, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(p: &str) -> Excludes {
        Excludes::new(&[p.to_string()])
    }

    #[test]
    fn names_and_paths() {
        assert!(ex("*.tmp").matches(b"a/b/x.tmp", false));
        assert!(!ex("*.tmp").matches(b"a/x.tmp/y", false));
        assert!(ex("cache/").matches(b"a/cache", true));
        assert!(!ex("cache/").matches(b"a/cache", false));
        assert!(ex("/top").matches(b"top", false));
        assert!(!ex("/top").matches(b"a/top", false));
        assert!(ex("b/c").matches(b"a/b/c", false));
        assert!(!ex("b/c").matches(b"a/xb/c", false));
        assert!(ex("a/**/z").matches(b"a/z", false));
        assert!(ex("a/**/z").matches(b"a/b/c/z", false));
        assert!(!ex("a/*/z").matches(b"a/b/c/z", false));
        assert!(ex("f0?[0-4]").matches(b"d/f012", false));
        assert!(!ex("f0?[0-4]").matches(b"d/f019", false));
        assert!(ex("[!a]*").matches(b"bx", false));
        assert!(!ex("[!a]*").matches(b"ax", false));
        assert!(ex("**.log").matches(b"a/b/c.log", false));
    }
}
