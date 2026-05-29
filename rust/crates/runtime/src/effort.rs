use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffortLevel {
    Ultracode,
    Xhigh,
    High,
    Medium,
    Low,
}

impl EffortLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ultracode => "ultracode",
            Self::Xhigh => "xhigh",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ultracode" | "ultra" => Some(Self::Ultracode),
            "xhigh" | "extra_high" | "max" => Some(Self::Xhigh),
            "high" => Some(Self::High),
            "medium" | "med" | "default" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }

    #[must_use]
    pub fn api_budget_tokens(self) -> Option<u32> {
        match self {
            Self::Ultracode => Some(64_000),
            Self::Xhigh => Some(32_000),
            Self::High => Some(10_000),
            Self::Medium => None,
            Self::Low => Some(3_200),
        }
    }

    #[must_use]
    pub fn all_levels() -> &'static [EffortLevel] {
        &[Self::Ultracode, Self::Xhigh, Self::High, Self::Medium, Self::Low]
    }
}

impl std::fmt::Display for EffortLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for EffortLevel {
    fn default() -> Self {
        Self::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::EffortLevel;

    #[test]
    fn parses_all_levels() {
        assert_eq!(EffortLevel::from_str("xhigh"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("high"), Some(EffortLevel::High));
        assert_eq!(EffortLevel::from_str("medium"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_str("low"), Some(EffortLevel::Low));
    }

    #[test]
    fn parses_aliases() {
        assert_eq!(EffortLevel::from_str("XHIGH"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("extra_high"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("max"), Some(EffortLevel::Xhigh));
        assert_eq!(EffortLevel::from_str("med"), Some(EffortLevel::Medium));
        assert_eq!(EffortLevel::from_str("default"), Some(EffortLevel::Medium));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(EffortLevel::from_str("extreme"), None);
        assert_eq!(EffortLevel::from_str(""), None);
    }

    #[test]
    fn display_matches_as_str() {
        for level in EffortLevel::all_levels() {
            assert_eq!(level.to_string(), level.as_str());
        }
    }

    #[test]
    fn default_is_medium() {
        assert_eq!(EffortLevel::default(), EffortLevel::Medium);
    }

    #[test]
    fn parses_ultracode() {
        assert_eq!(EffortLevel::from_str("ultracode"), Some(EffortLevel::Ultracode));
        assert_eq!(EffortLevel::from_str("ultra"), Some(EffortLevel::Ultracode));
        assert_eq!(EffortLevel::from_str("ULTRA"), Some(EffortLevel::Ultracode));
    }

    #[test]
    fn ultracode_has_highest_budget() {
        assert!(EffortLevel::Ultracode.api_budget_tokens() > EffortLevel::Xhigh.api_budget_tokens());
        assert_eq!(EffortLevel::Ultracode.api_budget_tokens(), Some(64_000));
    }

    #[test]
    fn display_ultracode() {
        assert_eq!(EffortLevel::Ultracode.to_string(), "ultracode");
    }

    #[test]
    fn api_budget_xhigh_is_highest() {
        assert!(EffortLevel::Xhigh.api_budget_tokens() > EffortLevel::High.api_budget_tokens());
    }

    #[test]
    fn api_budget_medium_is_none() {
        assert!(EffortLevel::Medium.api_budget_tokens().is_none());
    }

    #[test]
    fn roundtrip_serde() {
        for level in EffortLevel::all_levels() {
            let json = serde_json::to_string(level).unwrap();
            let parsed: EffortLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, parsed);
        }
    }
}
