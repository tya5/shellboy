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
    Some(Plan { target_tokens, remove_characters })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrated_removal_uses_the_paired_request_and_rounds_up() {
        assert_eq!(plan(200_000, 200_000, 800_000), Some(Plan { target_tokens: 160_000, remove_characters: 160_000 }));
        assert_eq!(plan(5, 6, 7).unwrap().remove_characters, 3);
        assert_eq!(plan(200_000, 0, 800_000), None);
        assert_eq!(plan(200_000, 200_000, 0), None);
        assert!(plan(u64::MAX, u64::MAX, u64::MAX).is_some());
    }
}
