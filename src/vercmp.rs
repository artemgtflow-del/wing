use std::cmp::Ordering;

/// Compares two winget version strings.
///
/// Rules (approximation of the WinGet semantics, enough for picking "latest"):
///   * segments are split on `.` and compared left to right;
///   * numeric vs numeric -> numeric compare;
///   * numeric vs alpha   -> numeric wins (a release beats a pre-release);
///   * alpha vs alpha     -> case-insensitive compare;
///   * extra segments that are all zero are ignored (1.0 == 1.0.0);
///   * otherwise more segments wins.
pub fn cmp(a: &str, b: &str) -> Ordering {
    let sa: Vec<&str> = a.trim().split('.').collect();
    let sb: Vec<&str> = b.trim().split('.').collect();

    let n = sa.len().max(sb.len());
    for i in 0..n {
        let x = sa.get(i).copied();
        let y = sb.get(i).copied();
        match (x, y) {
            (Some(x), Some(y)) => {
                let o = cmp_seg(x, y);
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), None) => {
                if !is_zero_seg(x) {
                    return Ordering::Greater;
                }
            }
            (None, Some(y)) => {
                if !is_zero_seg(y) {
                    return Ordering::Less;
                }
            }
            (None, None) => {}
        }
    }
    // Tie-break: 7.4.20 < 7.4.20.0 is false for winget (more precise is greater)
    sa.len().cmp(&sb.len())
}

fn is_zero_seg(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_digit()) && s.bytes().all(|c| c == b'0')
}

fn cmp_seg(x: &str, y: &str) -> Ordering {
    match (x.parse::<u128>(), y.parse::<u128>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => Ordering::Greater,
        (Err(_), Ok(_)) => Ordering::Less,
        (Err(_), Err(_)) => {
            let lx = x.to_ascii_lowercase();
            let ly = y.to_ascii_lowercase();
            match lx.cmp(&ly) {
                Ordering::Equal => x.cmp(y),
                other => other,
            }
        }
    }
}

/// Loose "is this newer?" used for ARP-detected versions.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    cmp(candidate, current) == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric() {
        assert_eq!(cmp("1.2.3", "1.2.4"), Ordering::Less);
        assert_eq!(cmp("7.5.0", "7.4.20"), Ordering::Greater);
        assert_eq!(cmp("1.10", "1.9"), Ordering::Greater);
        assert_eq!(cmp("2023.1.1", "2023.1.1"), Ordering::Equal);
    }

    #[test]
    fn prerelease() {
        assert_eq!(cmp("1.0.0-beta", "1.0.0"), Ordering::Less);
        assert_eq!(cmp("1.0.0-rc.1", "1.0.0-beta"), Ordering::Greater);
        assert_eq!(cmp("1.0.0-alpha", "1.0.0-beta"), Ordering::Less);
    }

    #[test]
    fn trailing_zeros() {
        assert_eq!(cmp("7.4.20", "7.4.20.0"), Ordering::Less);
        assert!(is_newer("2.0", "1.9.9"));
        assert!(!is_newer("1.0", "1.0"));
    }
}