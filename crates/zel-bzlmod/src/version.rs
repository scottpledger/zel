//! Port of `Version.java`: Bazel module versions with SemVer-like ordering where
//! prereleases sort before releases and the empty version sorts after every other version.

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Identifier {
    digits_only: bool,
    number: u64,
    text: String,
}

impl Identifier {
    fn from(s: &str) -> Result<Self, String> {
        if s.is_empty() {
            return Err("identifier is empty".into());
        }
        if s.chars().all(|c| c.is_ascii_digit()) {
            let number = s
                .parse::<u64>()
                .map_err(|_| format!("numeric version segment is too large: {s}"))?;
            Ok(Self { digits_only: true, number, text: s.to_string() })
        } else {
            Ok(Self { digits_only: false, number: 0, text: s.to_string() })
        }
    }
}

impl Ord for Identifier {
    fn cmp(&self, o: &Self) -> Ordering {
        // Digit-only identifiers sort first (trueFirst), then by number, then by string.
        o.digits_only
            .cmp(&self.digits_only)
            .then(self.number.cmp(&o.number))
            .then(self.text.cmp(&o.text))
    }
}
impl PartialOrd for Identifier {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// A module version. `Version::empty()` is used for the root module and non-registry overrides.
#[derive(Debug, Clone, Eq, Hash)]
pub struct Version {
    release: Vec<Identifier>,
    prerelease: Vec<Identifier>,
    normalized: String,
}

impl Version {
    pub fn empty() -> Self {
        Self { release: vec![], prerelease: vec![], normalized: String::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    pub fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }

    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    /// Parses per `Version.parse`: `release(-prerelease)?(+build)?`; build metadata is dropped.
    pub fn parse(version: &str) -> Result<Self, String> {
        if version.is_empty() {
            return Ok(Self::empty());
        }
        let bad = || format!("bad version (does not match regex): {version}");
        let (rest, build) = match version.split_once('+') {
            Some((r, b)) => (r, Some(b)),
            None => (version, None),
        };
        if let Some(b) = build {
            if b.is_empty() || !b.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
                return Err(bad());
            }
        }
        let (release, prerelease) = match rest.split_once('-') {
            Some((r, p)) => (r, Some(p)),
            None => (rest, None),
        };
        if release.is_empty() || !release.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
            return Err(bad());
        }
        if let Some(p) = prerelease {
            if p.is_empty()
                || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            {
                return Err(bad());
            }
        }
        let wrap = |e: String| format!("error parsing version: {version}: {e}");
        let mut rel = Vec::new();
        for id in release.split('.') {
            rel.push(Identifier::from(id).map_err(wrap)?);
        }
        let mut pre = Vec::new();
        if let Some(p) = prerelease {
            for id in p.split('.') {
                pre.push(Identifier::from(id).map_err(wrap)?);
            }
        }
        let normalized = match prerelease {
            Some(p) => format!("{release}-{p}"),
            None => release.to_string(),
        };
        Ok(Self { release: rel, prerelease: pre, normalized })
    }
}

impl PartialEq for Version {
    fn eq(&self, o: &Self) -> bool {
        self.normalized == o.normalized
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.is_empty()
            .cmp(&o.is_empty())
            .then_with(|| self.release.cmp(&o.release))
            // prerelease sorts before release (trueFirst)
            .then_with(|| o.is_prerelease().cmp(&self.is_prerelease()))
            .then_with(|| self.prerelease.cmp(&o.prerelease))
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering_matches_bazel() {
        assert!(v("1.0") < v("1.0.1"));
        assert!(v("1.9") < v("1.10"));
        assert!(v("1.0-rc1") < v("1.0"));
        assert!(v("1.0-alpha") < v("1.0-beta"));
        assert!(v("1.0-1") < v("1.0-alpha")); // numeric identifiers before strings
        assert!(v("1.0.bcr.1") > v("1.0")); // longer release wins after common prefix
        assert!(v("1.0.0") > v("1.0")); // lexicographic: prefix is smaller
        assert!(v("999") < Version::empty());
        assert_eq!(v("1.0+build5"), v("1.0"));
        assert_eq!(v("1.0-pre+b").to_string(), "1.0-pre");
        assert!(v("1.a") > v("1.99")); // string > numeric identifier
        assert!(v("10.0.0-prerelease") < v("10.0.0"));
    }

    #[test]
    fn rejects_bad() {
        assert!(Version::parse("1.0 x").is_err());
        assert!(Version::parse("1..0").is_err());
        assert!(Version::parse("1.0+").is_err());
        assert!(Version::parse("99999999999999999999").is_err());
        assert!(Version::parse("").unwrap().is_empty());
    }
}
