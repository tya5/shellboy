//! Calibrated planning from a single provider measurement and its request size.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub target_tokens: u64,
    pub remove_characters: u64,
}

pub(super) fn target_tokens(threshold: u64, percent: u8) -> u64 {
    ((threshold as u128 * percent as u128) / 100) as u64
}

pub(super) fn pending_matches(threshold: u64, percent: u8, recorded_percent: Option<u64>, recorded_target: u64) -> bool {
    recorded_percent == Some(percent as u64) && recorded_target == target_tokens(threshold, percent)
}

pub(super) fn plan(threshold: u64, percent: u8, input_tokens: u64, input_characters: u64) -> Option<Plan> {
    if input_tokens == 0 || input_characters == 0 {
        return None;
    }
    let target_tokens = target_tokens(threshold, percent);
    let excess = input_tokens.saturating_sub(target_tokens) as u128;
    let chars = excess * input_characters as u128;
    let remove_characters = chars.div_ceil(input_tokens as u128).min(u64::MAX as u128) as u64;
    Some(Plan {
        target_tokens,
        remove_characters,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verification {
    Met,
    Insufficient,
}

pub(super) fn verify(target: u64, input: u64) -> Verification {
    if input <= target {
        Verification::Met
    } else {
        Verification::Insufficient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_target_rounding_identity_and_extremes() {
        assert_eq!(target_tokens(600_000,20),120_000);
        assert_eq!(plan(600_000,20,600_000,2_400_000).unwrap().remove_characters,1_920_000);
        assert_eq!(target_tokens(u64::MAX,100),u64::MAX);
        assert_eq!(target_tokens(1,20),0);
        assert_eq!(target_tokens(0,20),0);
        assert_eq!(plan(600_000,100,600_000,2_400_000).unwrap().remove_characters,0);
        assert!(!pending_matches(1,21,Some(20),0));
        assert!(pending_matches(1,20,Some(20),0));
        assert!(!pending_matches(1,20,None,0));
    }
    #[test]
    fn calibrated_removal_uses_the_paired_request_and_rounds_up() {
        assert_eq!(
            plan(200_000, 80, 200_000, 800_000),
            Some(Plan {
                target_tokens: 160_000,
                remove_characters: 160_000
            })
        );
        assert_eq!(plan(5, 80, 6, 7).unwrap().remove_characters, 3);
        assert_eq!(plan(200_000, 80, 0, 800_000), None);
        assert_eq!(plan(200_000, 80, 200_000, 0), None);
        assert!(plan(u64::MAX, 80, u64::MAX, u64::MAX).is_some());
    }
    #[test]
    fn scripted_loop_escalates_on_the_first_fresh_usage() {
        let threshold = 200_000;
        let first = plan(threshold, 80, 201_061, 804_244).unwrap();
        assert_eq!(first.target_tokens, 160_000);
        assert_eq!(first.remove_characters, 164_244);
        // The old policy waited for 3-8 requests until 200K was crossed again.
        // A 184K fresh report is already insufficient and selects a wider plan.
        let usage = [184_622, 188_000, 195_000, 203_895];
        assert_eq!(
            verify(first.target_tokens, usage[0]),
            Verification::Insufficient
        );
        assert!(usage[0] < threshold);
        assert_eq!(verify(first.target_tokens, 159_999), Verification::Met);
    }
}
