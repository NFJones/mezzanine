//! Product-friendly subagent display names.
//!
//! Canonical subagent domain records live in `mez-agent`; this module contains
//! only the human-readable names used by product panes and status lines.

use std::sync::LazyLock;

/// Built-in nonhuman subagent display names embedded in the product binary.
///
/// The reference corpus remains outside the tracked product source tree. This
/// lazy collection turns the compile-time embedded text into individual names
/// only when display-name allocation first needs it.
#[allow(dead_code)]
pub static SUBAGENT_NONHUMAN_NAMES: LazyLock<Vec<&str>> =
    LazyLock::new(|| include_str!("nonhuman_names.txt").lines().collect());

/// Built-in human-readable subagent display names.
///
/// The list deliberately uses short, familiar first names so subagent panes and
/// parent status lines stay compact while remaining easier to distinguish than
/// canonical runtime ids such as `agent-%2`.
pub const SUBAGENT_HUMAN_NAMES: &[&str] = &[
    "Alice",
    "Bob",
    "Sally",
    "Charlie",
    "Dana",
    "Emily",
    "Frank",
    "Grace",
    "Hannah",
    "Isaac",
    "Jack",
    "Kate",
    "Liam",
    "Mia",
    "Noah",
    "Olivia",
    "Paul",
    "Quinn",
    "Rachel",
    "Sam",
    "Tara",
    "Uma",
    "Victor",
    "Wendy",
    "Xavier",
    "Yvonne",
    "Zach",
    "Aaron",
    "Abigail",
    "Adam",
    "Alexis",
    "Allison",
    "Amanda",
    "Amy",
    "Andrew",
    "Angela",
    "Anna",
    "Anthony",
    "Ashley",
    "Austin",
    "Barbara",
    "Ben",
    "Beth",
    "Blake",
    "Brandon",
    "Brian",
    "Brittany",
    "Brooke",
    "Caleb",
    "Cameron",
    "Carolyn",
    "Catherine",
    "Chloe",
    "Chris",
    "Christina",
    "Claire",
    "Cody",
    "Colin",
    "Connor",
    "Daniel",
    "Danielle",
    "David",
    "Debra",
    "Denise",
    "Diana",
    "Dylan",
    "Edward",
    "Elizabeth",
    "Emma",
    "Eric",
    "Ethan",
    "Evelyn",
    "Gary",
    "George",
    "Heather",
    "Henry",
    "Isabella",
    "Jacob",
    "James",
    "Jason",
    "Jennifer",
    "Jessica",
    "John",
    "Jordan",
    "Joseph",
    "Joshua",
    "Julia",
    "Justin",
    "Karen",
    "Kelly",
    "Kevin",
    "Kimberly",
    "Kyle",
    "Laura",
    "Lauren",
    "Leah",
    "Linda",
    "Lisa",
    "Madison",
    "Mark",
    "Mary",
    "Megan",
    "Melissa",
    "Michael",
    "Michelle",
    "Morgan",
    "Natalie",
    "Nathan",
    "Nicole",
    "Patrick",
    "Rebecca",
    "Robert",
    "Ryan",
    "Sarah",
    "Scott",
    "Sean",
    "Stephanie",
    "Steven",
    "Susan",
    "Taylor",
    "Thomas",
    "Tiffany",
    "Tyler",
    "Victoria",
    "Adrian",
    "Aiden",
    "Aisha",
    "Amara",
    "Anika",
    "Aria",
    "Asher",
    "Avery",
    "Bianca",
    "Camila",
    "Carmen",
    "Cecilia",
    "Diego",
    "Elena",
    "Eli",
    "Elias",
    "Felix",
    "Gabriel",
    "Harper",
    "Imani",
    "Iris",
    "Jade",
    "Jamal",
    "Jasmine",
    "Kai",
    "Layla",
    "Leo",
    "Lila",
    "Logan",
    "Luca",
    "Marcus",
    "Maya",
    "Miles",
    "Nadia",
    "Nina",
    "Omar",
    "Parker",
    "Priya",
    "Ravi",
    "Reese",
    "Riley",
    "Rowan",
    "Sage",
    "Sofia",
    "Theo",
    "Valeria",
    "Zara",
    "Zoe",
];

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{SUBAGENT_HUMAN_NAMES, SUBAGENT_NONHUMAN_NAMES};

    #[test]
    /// Verifies the compile-time embedded nonhuman corpus retains the complete
    /// 8,192-entry source sequence without duplicate entries or accidental
    /// whitespace transformations.
    ///
    /// The production allocator consumes these names as a finite corpus. A
    /// count-only check would permit reordered, repeated, or substituted names,
    /// so this test also fingerprints the exact newline-delimited representation
    /// that the checked-in reference artifact defines.
    fn nonhuman_name_corpus_preserves_required_complete_ordered_contents() {
        assert_eq!(SUBAGENT_NONHUMAN_NAMES.len(), 8_192);
        assert_eq!(
            SUBAGENT_NONHUMAN_NAMES
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            8_192
        );

        let mut hasher = Sha256::new();
        for name in SUBAGENT_NONHUMAN_NAMES.iter() {
            hasher.update(name.as_bytes());
            hasher.update(b"\n");
        }
        assert_eq!(
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "21533e3887d835b704fc6d1fa271fd49a12d8452213dfe220238cf38197cf9f3"
        );
    }

    #[test]
    /// Verifies every embedded nonhuman name satisfies the product's semantic
    /// display-name constraints independently of the accepted corpus hash.
    ///
    /// The fingerprint above protects the current ordered contents, while these
    /// assertions keep a future intentional corpus replacement from admitting
    /// empty, non-ASCII, serial-numbered, or reserved bot/unit-like names.
    fn nonhuman_name_corpus_satisfies_display_name_invariants() {
        for name in SUBAGENT_NONHUMAN_NAMES.iter() {
            assert!(!name.is_empty(), "nonhuman names must not be empty");
            assert!(
                name.bytes().all(|byte| byte.is_ascii_alphabetic()),
                "nonhuman name must be ASCII alphabetic: {name}"
            );
            assert!(
                name.as_bytes()[0].is_ascii_uppercase(),
                "nonhuman name must start with an uppercase ASCII letter: {name}"
            );
            let lowercase = name.to_ascii_lowercase();
            assert!(
                !lowercase.contains("bot") && !lowercase.contains("unit"),
                "nonhuman name must not contain reserved bot/unit text: {name}"
            );
            assert!(
                !name.as_bytes().last().is_some_and(u8::is_ascii_digit),
                "nonhuman name must not have a numeric serial suffix: {name}"
            );
            assert!(
                !name
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'-' && pair[1].is_ascii_digit()),
                "nonhuman name must not contain a hyphen-number fragment: {name}"
            );
        }
    }

    #[test]
    /// Verifies the established human display-name corpus stays untouched while
    /// its public symbol is renamed to distinguish it from the nonhuman corpus.
    ///
    /// The allocator's random selection and exhaustion behavior depend on this
    /// ordered sequence. Hashing the names separated by newlines, without a
    /// terminal separator, detects any literal, count, or ordering change.
    fn human_name_corpus_preserves_required_ordered_contents() {
        assert_eq!(SUBAGENT_HUMAN_NAMES.len(), 172);

        let mut hasher = Sha256::new();
        for (index, name) in SUBAGENT_HUMAN_NAMES.iter().enumerate() {
            if index != 0 {
                hasher.update(b"\n");
            }
            hasher.update(name.as_bytes());
        }
        assert_eq!(
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "446c6f7cc1876281dc1b29337af08b1ad99ff447722ed05e2555f98531d3978d"
        );
    }
}
