use std::collections::HashSet;

use rand::SeedableRng;
use tnld::random_subdomain::{generate_unique, Reserved};

#[derive(Default)]
struct FakeRegistry {
    taken: HashSet<String>,
}

impl Reserved for FakeRegistry {
    fn contains(&self, s: &str) -> bool {
        self.taken.contains(s)
    }
}

#[test]
fn generated_subdomains_match_dns_regex() {
    let re = regex::Regex::new(r"^[a-z][a-z0-9-]{1,30}[a-z0-9]$").unwrap();
    let reg = FakeRegistry::default();
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    for _ in 0..100 {
        let s = generate_unique(&mut rng, &reg).expect("non-colliding");
        assert!(re.is_match(&s), "fails regex: {s}");
    }
}

#[test]
fn no_duplicates_in_a_run_of_50() {
    let mut reg = FakeRegistry::default();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    for _ in 0..50 {
        let s = generate_unique(&mut rng, &reg).expect("non-colliding");
        assert!(
            reg.taken.insert(s.clone()),
            "duplicate within 50 attempts: {s}"
        );
    }
}
