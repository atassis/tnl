//! Wordlists for ngrok-style memorable random subdomains.
//!
//! Combinations: 64 × 64 × 99 ≈ 405k. Single-word, lowercase, no profanity.
//! When you change these lists, re-run `cargo test -p tnl-protocol --lib
//! wordlist_safety` so the curated forbidden list still passes.

pub const ADJECTIVES: &[&str] = &[
    "amber", "ancient", "azure", "blue", "bold", "brave", "bright", "calm", "clever", "cool",
    "cosmic", "crisp", "curly", "daring", "deep", "easy", "fancy", "fast", "fine", "fluffy",
    "free", "fresh", "gentle", "glad", "golden", "grand", "green", "happy", "honest", "humble",
    "icy", "jolly", "keen", "kind", "lively", "loyal", "lucky", "mellow", "merry", "mighty",
    "modest", "neat", "noble", "polite", "proud", "quick", "quiet", "ready", "regal", "rich",
    "shiny", "silver", "smart", "smooth", "soft", "solid", "sunny", "sweet", "swift", "tidy",
    "trim", "warm", "wise", "young",
];

pub const NOUNS: &[&str] = &[
    "apple", "arrow", "badger", "berry", "bird", "boat", "branch", "breeze", "brook", "canyon",
    "cedar", "cliff", "cloud", "coast", "creek", "delta", "dolphin", "dune", "eagle", "ember",
    "falcon", "fern", "field", "fjord", "forest", "fox", "frog", "garden", "glade", "grove",
    "harbor", "hawk", "hill", "horizon", "island", "ivy", "lake", "leaf", "lily", "lion", "marsh",
    "meadow", "moon", "moss", "mountain", "ocean", "orchid", "otter", "owl", "panda", "pine",
    "plain", "pond", "prairie", "rain", "raven", "river", "rose", "shore", "sky", "spring", "star",
    "tide", "tundra",
];

const _: () = {
    // Compile-time assertion that lists are sized as documented.
    assert!(ADJECTIVES.len() == 64);
    assert!(NOUNS.len() == 64);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_have_expected_size() {
        assert_eq!(ADJECTIVES.len(), 64);
        assert_eq!(NOUNS.len(), 64);
    }

    #[test]
    fn entries_are_lowercase_single_word_alpha() {
        for w in ADJECTIVES.iter().chain(NOUNS.iter()) {
            assert!(
                w.chars().all(|c| c.is_ascii_lowercase()),
                "non-lowercase: {w}"
            );
            assert!(!w.contains(' '), "multi-word entry: {w}");
            assert!(!w.is_empty(), "empty entry");
            assert!(w.len() <= 12, "too long for memorable subdomain: {w}");
        }
    }

    #[test]
    fn wordlist_safety_no_profanity() {
        // Curated forbidden-substring list. Conservative; extend on demand.
        const FORBIDDEN: &[&str] = &[
            "ass", "fck", "fuk", "shit", "cunt", "slut", "whore", "nazi", "rape", "kill", "die",
        ];
        for w in ADJECTIVES.iter().chain(NOUNS.iter()) {
            for bad in FORBIDDEN {
                assert!(
                    !w.contains(bad),
                    "wordlist entry {w:?} contains forbidden substring {bad:?}"
                );
            }
        }
    }

    #[test]
    fn no_duplicates_within_each_list() {
        let mut adj: Vec<_> = ADJECTIVES.to_vec();
        adj.sort_unstable();
        adj.dedup();
        assert_eq!(adj.len(), ADJECTIVES.len(), "duplicate adjective");
        let mut noun: Vec<_> = NOUNS.to_vec();
        noun.sort_unstable();
        noun.dedup();
        assert_eq!(noun.len(), NOUNS.len(), "duplicate noun");
    }
}
