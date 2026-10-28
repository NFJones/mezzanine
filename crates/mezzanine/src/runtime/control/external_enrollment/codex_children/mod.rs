//! Original native Codex child bindings, never parent presentation or authority.
//!
//! Only actor-settled SubagentStart under the exact original parent session can
//! bind a child. Identities and turn tombstones are finite and never evicted;
//! Stop is a continuable turn boundary, never proof that a child has ended.
//! All requests share the parent's existing custody budget and durable owner.

use super::*;
use std::collections::BTreeMap;

/// Finite original child identities retained until their parent/source ends.
const CHILDREN: usize = 128;
/// Aggregate child turn fences per parent, independent of request custody.
const TURNS: usize = 4096;

/// One explicitly started child's original start and non-revivable turn history.
#[derive(Debug)]
struct Child {
    initial: String,
    current: String,
    seen: BTreeSet<String>,
    last_stop: Option<String>,
}

/// Runtime-only original-epoch child state; no process, lease or billing rights.
#[derive(Debug, Default)]
pub(super) struct Children {
    children: BTreeMap<String, Child>,
    turns: usize,
}

impl Children {
    /// Applies only an already native-qualified lifecycle event. Exact start and
    /// stop repeats are inert; unknown stops and changed original starts reject.
    pub(super) fn observe(&mut self, kind: &str, id: &str, turn: &str) -> Result<bool> {
        if kind == "SubagentStop" {
            let child = self
                .children
                .get_mut(id)
                .ok_or_else(|| MezError::forbidden("Codex original child start unavailable"))?;
            let changed = child.last_stop.as_deref() != Some(turn);
            child.last_stop = Some(turn.into());
            return Ok(changed);
        }
        if kind != "SubagentStart" {
            return Err(MezError::invalid_args("Codex child lifecycle unavailable"));
        }
        if let Some(child) = self.children.get(id) {
            return if child.initial == turn {
                Ok(false)
            } else {
                Err(MezError::conflict(
                    "Codex original child cannot be replaced",
                ))
            };
        }
        if self.children.len() >= CHILDREN || self.turns >= TURNS {
            return Err(MezError::forbidden(
                "Codex child fence capacity unavailable",
            ));
        }
        self.children.insert(
            id.into(),
            Child {
                initial: turn.into(),
                current: turn.into(),
                seen: BTreeSet::from([turn.into()]),
                last_stop: None,
            },
        );
        self.turns += 1;
        Ok(true)
    }

    /// Checks future readiness only. Older admitted outcomes may settle after a
    /// child reaches Stop or advances; Stop may continue on the same turn.
    pub(super) fn allows_pre(&self, id: &str, turn: &str) -> bool {
        self.children.get(id).is_some_and(|child| {
            child.current == turn || (!child.seen.contains(turn) && self.turns < TURNS)
        })
    }

    /// Advances child turn history only with native worker-published readiness.
    /// The shared serving ticket prevents lifecycle mutation between planning
    /// and this commit; rejection never grants an origin to a different child.
    pub(super) fn admit_turn(&mut self, id: &str, turn: &str) -> Result<()> {
        if !self.allows_pre(id, turn) {
            return Err(MezError::forbidden("Codex child readiness unavailable"));
        }
        let child = self
            .children
            .get_mut(id)
            .ok_or_else(|| MezError::forbidden("Codex original child start unavailable"))?;
        if child.seen.insert(turn.into()) {
            self.turns += 1;
        }
        child.current = turn.into();
        Ok(())
    }
}

/// Separates children and siblings from every valid raw main-turn selector.
/// '@' cannot occur in the public main grammar; only an opaque hash follows it.
pub(super) fn turn_key(child: &str, turn: &str) -> String {
    let mut hash = Sha256::new();
    for part in ["codex-child-turn/1", child, turn] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!(
        "@child:{}",
        hash.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Start and turn fences cannot reassign a child or adopt an old turn.
    /// Child/sibling keys remain disjoint even with identical public turn IDs;
    /// exhausting identities or aggregate turns never evicts existing fences.
    #[test]
    fn codex_child_bindings_preserve_original_start_and_bounded_turn_fences() {
        let mut owner = Children::default();
        assert!(!owner.allows_pre("a", "same"));
        assert!(owner.observe("SubagentStop", "a", "same").is_err());
        assert!(owner.observe("SubagentStart", "a", "same").unwrap());
        assert!(!owner.observe("SubagentStart", "a", "same").unwrap());
        assert!(owner.observe("SubagentStart", "a", "changed").is_err());
        owner.admit_turn("a", "later").unwrap();
        assert!(!owner.allows_pre("a", "same"));
        assert!(owner.admit_turn("a", "same").is_err());
        assert!(owner.observe("SubagentStop", "a", "later").unwrap());
        assert!(!owner.observe("SubagentStop", "a", "later").unwrap());
        assert!(owner.allows_pre("a", "later"));
        assert!(!owner.observe("SubagentStart", "a", "same").unwrap());
        for index in 1..CHILDREN {
            owner
                .observe("SubagentStart", &format!("child-{index}"), "same")
                .unwrap();
        }
        assert!(owner.observe("SubagentStart", "overflow", "same").is_err());
        for index in owner.turns..TURNS {
            owner
                .admit_turn("child-1", &format!("turn-{index}"))
                .unwrap();
        }
        assert!(!owner.allows_pre("child-1", "overflow"));
        assert!(owner.allows_pre("child-1", &format!("turn-{}", TURNS - 1)));
        assert_eq!(owner.children.len(), CHILDREN);
        assert_ne!(turn_key("a", "same"), turn_key("b", "same"));
        assert!(turn_key("a", "same").starts_with('@'));
    }
}
