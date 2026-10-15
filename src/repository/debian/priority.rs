use std::cmp::Ordering;
use std::str::FromStr;

/// Package priority level.
///
/// Priorities are ordered from highest to lowest:
/// Required > Important > Standard > Optional > Extra
///
/// Ref: https://www.debian.org/doc/debian-policy/ch-archive.html#priorities
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Required,
    Important,
    Standard,
    Optional,
    Extra,
}

impl Priority {
    /// Returns a numeric rank for sorting (higher = more important).
    fn rank(self) -> u8 {
        match self {
            Priority::Required => 5,
            Priority::Important => 4,
            Priority::Standard => 3,
            Priority::Optional => 2,
            Priority::Extra => 1,
        }
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

#[derive(Debug)]
pub struct ParsePriorityError {
    pub unknown_priority: String,
}

impl FromStr for Priority {
    type Err = ParsePriorityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "required" => Ok(Priority::Required),
            "important" => Ok(Priority::Important),
            "standard" => Ok(Priority::Standard),
            "optional" => Ok(Priority::Optional),
            "extra" => Ok(Priority::Extra),
            _ => Err(ParsePriorityError {
                unknown_priority: s.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::Required => write!(f, "required"),
            Priority::Important => write!(f, "important"),
            Priority::Standard => write!(f, "standard"),
            Priority::Optional => write!(f, "optional"),
            Priority::Extra => write!(f, "extra"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::Required > Priority::Important);
        assert!(Priority::Important > Priority::Standard);
        assert!(Priority::Standard > Priority::Optional);
        assert!(Priority::Optional > Priority::Extra);
    }

    #[test]
    fn test_priority_sort() {
        let mut priorities = vec![
            Priority::Optional,
            Priority::Required,
            Priority::Extra,
            Priority::Standard,
            Priority::Important,
        ];
        priorities.sort();
        priorities.reverse(); // Highest first
        assert_eq!(
            priorities,
            vec![
                Priority::Required,
                Priority::Important,
                Priority::Standard,
                Priority::Optional,
                Priority::Extra,
            ]
        );
    }

    #[test]
    fn test_priority_from_str() {
        assert_eq!(Priority::from_str("required").unwrap(), Priority::Required);
        assert_eq!(
            Priority::from_str("important").unwrap(),
            Priority::Important
        );
        assert_eq!(Priority::from_str("standard").unwrap(), Priority::Standard);
        assert_eq!(Priority::from_str("optional").unwrap(), Priority::Optional);
        assert_eq!(Priority::from_str("extra").unwrap(), Priority::Extra);
        assert!(Priority::from_str("unknown").is_err());
    }
}
