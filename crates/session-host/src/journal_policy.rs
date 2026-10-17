//! Journal event policy, separate from terminal semantic checkpoint schema.
//!
//! Production launches remain on the legacy profile until conservative resource
//! preflight and aggregate reservations are implemented. The new reader policy
//! makes durable refusal fixtures unambiguous without changing old journals.
use pigade_terminal::CORE_PROFILE;

pub const RESOURCE_REFUSAL_SUFFIX: &str = ";journal=resource-refusal-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Legacy,
    ResourceRefusalV1,
}
impl Policy {
    pub fn from_profile(profile: &str) -> Option<Self> {
        if profile == CORE_PROFILE {
            Some(Self::Legacy)
        } else if profile.strip_suffix(RESOURCE_REFUSAL_SUFFIX) == Some(CORE_PROFILE) {
            Some(Self::ResourceRefusalV1)
        } else {
            None
        }
    }
}

pub fn resource_refusal_profile() -> String {
    format!("{CORE_PROFILE}{RESOURCE_REFUSAL_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_policy_does_not_accept_a_different_core_or_loose_suffix() {
        assert_eq!(Policy::from_profile(CORE_PROFILE), Some(Policy::Legacy));
        let new = resource_refusal_profile();
        assert_eq!(Policy::from_profile(&new), Some(Policy::ResourceRefusalV1));
        for bad in [
            format!("{new};extra"),
            format!("other{RESOURCE_REFUSAL_SUFFIX}"),
            format!("{new}{RESOURCE_REFUSAL_SUFFIX}"),
        ] {
            assert_eq!(Policy::from_profile(&bad), None);
        }
    }
}
