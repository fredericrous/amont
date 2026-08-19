//! A guard for prose no compiler reads.
//!
//! "Twenty checks" is load-bearing copy: it appears on the README's first
//! screen, on the docs landing page, at the top of the checks reference and in
//! the comparison table — the four pages a prospective user actually reads.
//! Nothing ties that word to `registry::CHECKS`, so the twenty-first check
//! would ship with every page quietly understating the count. Same shape as
//! `release_profile.rs`: the only place the question can be asked is the file
//! itself, found relative to this crate rather than to the process's working
//! directory.
//!
//! Design records are deliberately not checked — they are dated records of a
//! decision, and "the 20 checks of the day" in one of them is history, not a
//! claim about the present.

use amont_runtime::registry::CHECKS;

/// A page, found relative to this crate.
fn page(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The registry count, spelled the way prose spells it. Extending this map is
/// the deliberate speed bump: adding a check means saying its number out loud.
fn spelled(n: usize) -> &'static str {
    match n {
        18 => "eighteen",
        19 => "nineteen",
        20 => "twenty",
        21 => "twenty-one",
        22 => "twenty-two",
        23 => "twenty-three",
        24 => "twenty-four",
        25 => "twenty-five",
        26 => "twenty-six",
        27 => "twenty-seven",
        28 => "twenty-eight",
        29 => "twenty-nine",
        30 => "thirty",
        31 => "thirty-one",
        32 => "thirty-two",
        _ => panic!("teach spelled() the word for {n} while updating the prose"),
    }
}

/// Every user-facing page that states the check count states the current one.
#[test]
fn the_prose_count_matches_the_registry() {
    let word = spelled(CHECKS.len());
    for rel in ["README.md", "docs/index.md", "docs/checks.md"] {
        assert!(
            page(rel).to_lowercase().contains(word),
            "{rel} does not say \"{word}\" anywhere, but the registry has \
             {} checks — the page is stating a stale count",
            CHECKS.len()
        );
    }
    let digit = format!("{} built-in checks", CHECKS.len());
    assert!(
        page("docs/similar-projects.md").contains(&digit),
        "docs/similar-projects.md's comparison table does not say \
         \"{digit}\" — the table is stating a stale count"
    );
}

/// The guard above only bites if a stale spelling would actually be absent:
/// no page may hedge with two different number words for the checks.
#[test]
fn no_page_states_a_neighbouring_count() {
    let stale: Vec<&str> = (18..=25)
        .filter(|&n| n != CHECKS.len())
        .map(spelled)
        .collect();
    for rel in ["README.md", "docs/index.md", "docs/checks.md"] {
        let text = page(rel).to_lowercase();
        for word in &stale {
            for phrase in [format!("{word} checks"), format!("{word} built-in checks")] {
                assert!(
                    !text.contains(&phrase),
                    "{rel} says \"{phrase}\" while the registry has {} — \
                     one of them is wrong",
                    CHECKS.len()
                );
            }
        }
    }
}
