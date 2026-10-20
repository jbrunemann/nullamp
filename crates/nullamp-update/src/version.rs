//! Version numbers as release tags write them: `1.4.1`, `v2.0`, `1.5.0-rc.1`.

use std::cmp::Ordering;

/// Dotted numbers with an optional pre-release (`-rc.1`), ordered as
/// semver orders them, except that a missing number counts as 0 (`1.5` is
/// `1.5.0`). A `+build` part is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    numbers: Vec<u64>,
    pre: Option<String>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_owned()).filter(|p| !p.is_empty())),
            None => (text, None),
        };
        let mut numbers = core.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u64>>>()?;
        // Without the trailing zeros `1.5` and `1.5.0` are equal as values too, not only in order.
        while numbers.last() == Some(&0) {
            numbers.pop();
        }
        Some(Version { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.numbers.len().max(other.numbers.len());
        let number = |v: &Version, i: usize| v.numbers.get(i).copied().unwrap_or(0);
        (0..len)
            .map(|i| number(self, i).cmp(&number(other, i)))
            .find(|o| o.is_ne())
            .unwrap_or_else(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // A release comes after its pre-releases.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => compare_pre(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dot-separated parts: numbers by value and before words, words as text,
/// and the shorter list first when one is the start of the other.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.split('.'), b.split('.'));
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order.is_ne() {
                    return order;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn numbers_compare_by_value_not_as_text() {
        assert!(v("1.10.0") > v("1.9.9"));
        assert!(v("v2.0.0") > v("1.99.99"));
        assert_eq!(v("1.5"), v("1.5.0"));
        assert!(v("1.5.1") > v("1.5"));
    }

    #[test]
    fn a_pre_release_comes_before_its_release() {
        assert!(v("1.5.0-rc.1") < v("1.5.0"));
        assert!(v("1.5.0-rc.2") > v("1.5.0-rc.1"));
        assert!(v("1.5.0-rc.10") > v("1.5.0-rc.2"));
        assert!(v("1.5.0-rc") < v("1.5.0-rc.1"));
        assert!(v("1.5.0-beta") < v("1.5.0-rc"));
        assert!(v("1.4.9") < v("1.5.0-rc.1"));
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert_eq!(v("1.5.0+abc"), v("1.5.0"));
    }

    #[test]
    fn things_that_are_not_versions_are_refused() {
        for text in ["", "nightly", "1..2", "1.x", "v", "latest-1.0"] {
            assert_eq!(Version::parse(text), None, "{text}");
        }
    }
}
