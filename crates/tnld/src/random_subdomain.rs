use rand::seq::SliceRandom;
use rand::Rng;
use tnl_protocol::wordlists::{ADJECTIVES, NOUNS};

pub trait Reserved {
    fn contains(&self, s: &str) -> bool;
}

/// Try up to 5 times to generate `adj-noun-N` (N in 1..=99) that isn't taken.
/// Returns `None` if all 5 attempts collided (~1 in 405k chance per attempt).
pub fn generate_unique<R: Rng + ?Sized, Q: Reserved>(rng: &mut R, q: &Q) -> Option<String> {
    for _ in 0..5 {
        let adj = ADJECTIVES.choose(rng)?;
        let noun = NOUNS.choose(rng)?;
        let n: u32 = rng.gen_range(1..=99);
        let candidate = format!("{adj}-{noun}-{n}");
        if !q.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}
