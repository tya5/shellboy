//! Calibrated planning from a single provider measurement and its request size.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub target_tokens: u64,
    pub remove_characters: u64,
}

pub(super) fn plan(threshold: u64, input_tokens: u64, input_characters: u64) -> Option<Plan> {
    if input_tokens == 0 || input_characters == 0 {
        return None;
    }
    let target_tokens = ((threshold as u128 * 4) / 5) as u64;
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
    fn calibrated_removal_uses_the_paired_request_and_rounds_up() {
        assert_eq!(
            plan(200_000, 200_000, 800_000),
            Some(Plan {
                target_tokens: 160_000,
                remove_characters: 160_000
            })
        );
        assert_eq!(plan(5, 6, 7).unwrap().remove_characters, 3);
        assert_eq!(plan(200_000, 0, 800_000), None);
        assert_eq!(plan(200_000, 200_000, 0), None);
        assert!(plan(u64::MAX, u64::MAX, u64::MAX).is_some());
    }
    #[test]
    fn scripted_loop_escalates_on_the_first_fresh_usage() {
        let threshold = 200_000;
        let first = plan(threshold, 201_061, 804_244).unwrap();
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
