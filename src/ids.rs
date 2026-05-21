//! Stable identity generation.
//!
//! Mezzanine uses opaque stable identifiers for sessions, windows, panes,
//! clients, observers, and agents. The prefixes here follow the specification's
//! user-facing conventions while preserving opacity for callers.

use std::fmt::{self, Display, Formatter};

/// Carries Stable Id state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StableId(String);

impl StableId {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(prefix: char, value: u64) -> Self {
        Self(format!("{prefix}{value}"))
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn parse(expected_prefix: char, value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let mut chars = value.chars();
        if chars.next()? != expected_prefix || chars.as_str().is_empty() {
            return None;
        }
        chars.as_str().parse::<u64>().ok()?;
        Some(Self(value))
    }

    /// Runs the opaque operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn opaque(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return None;
        }
        Some(Self(value))
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Runs the numeric suffix operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn numeric_suffix(&self) -> Option<(char, u64)> {
        let mut chars = self.0.chars();
        let prefix = chars.next()?;
        let suffix = chars.as_str().parse::<u64>().ok()?;
        Some((prefix, suffix))
    }
}

impl Display for StableId {
    /// Runs the fmt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Defines the Session Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type SessionId = StableId;
/// Defines the Window Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type WindowId = StableId;
/// Defines the Window Group Id type used by this subsystem.
///
/// Window groups use the shell-safe `g` prefix so users can target them from
/// the Mezzanine command language without quoting common shell metacharacters.
pub type WindowGroupId = StableId;
/// Defines the Pane Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type PaneId = StableId;
/// Defines the Client Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type ClientId = StableId;
/// Defines the Observer Request Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type ObserverRequestId = StableId;
/// Defines the Agent Id type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub type AgentId = StableId;

/// Carries Id Factory state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct IdFactory {
    /// Stores the next session value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_session: u64,
    /// Stores the next window value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_window: u64,
    /// Stores the next window group value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_window_group: u64,
    /// Stores the next pane value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_pane: u64,
    /// Stores the next client value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_client: u64,
    /// Stores the next observer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_observer: u64,
    /// Stores the next agent value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    next_agent: u64,
}

impl Default for IdFactory {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            next_session: 1,
            next_window: 1,
            next_window_group: 1,
            next_pane: 1,
            next_client: 1,
            next_observer: 1,
            next_agent: 1,
        }
    }
}

impl IdFactory {
    /// Runs the after existing ids operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn after_existing_ids<'a>(ids: impl IntoIterator<Item = &'a StableId>) -> Self {
        let mut factory = Self::default();
        for id in ids {
            let Some((prefix, suffix)) = id.numeric_suffix() else {
                continue;
            };
            let next = suffix.saturating_add(1);
            match prefix {
                '$' => factory.next_session = factory.next_session.max(next),
                '@' => factory.next_window = factory.next_window.max(next),
                'g' => factory.next_window_group = factory.next_window_group.max(next),
                '%' => factory.next_pane = factory.next_pane.max(next),
                'c' => factory.next_client = factory.next_client.max(next),
                'o' => factory.next_observer = factory.next_observer.max(next),
                'a' => factory.next_agent = factory.next_agent.max(next),
                _ => {}
            }
        }
        factory
    }

    /// Runs the session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session(&mut self) -> SessionId {
        let id = StableId::new('$', self.next_session);
        self.next_session += 1;
        id
    }

    /// Runs the window operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn window(&mut self) -> WindowId {
        let id = StableId::new('@', self.next_window);
        self.next_window += 1;
        id
    }

    /// Returns the next stable window-group id.
    pub fn window_group(&mut self) -> WindowGroupId {
        let id = StableId::new('g', self.next_window_group);
        self.next_window_group += 1;
        id
    }

    /// Runs the pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane(&mut self) -> PaneId {
        let id = StableId::new('%', self.next_pane);
        self.next_pane += 1;
        id
    }

    /// Runs the client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn client(&mut self) -> ClientId {
        let id = StableId::new('c', self.next_client);
        self.next_client += 1;
        id
    }

    /// Runs the observer request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn observer_request(&mut self) -> ObserverRequestId {
        let id = StableId::new('o', self.next_observer);
        self.next_observer += 1;
        id
    }

    /// Runs the agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent(&mut self) -> AgentId {
        let id = StableId::new('a', self.next_agent);
        self.next_agent += 1;
        id
    }
}

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests {

    use super::{IdFactory, StableId};
    /// Verifies stable ids use spec prefixes.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn stable_ids_use_spec_prefixes() {
        let mut ids = IdFactory::default();

        assert_eq!(ids.session().as_str(), "$1");
        assert_eq!(ids.window_group().as_str(), "g1");
        assert_eq!(ids.window().as_str(), "@1");
        assert_eq!(ids.pane().as_str(), "%1");
        assert_eq!(ids.window().as_str(), "@2");
        assert_eq!(ids.pane().as_str(), "%2");
    }

    /// Verifies stable ids parse expected prefixes.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn stable_ids_parse_expected_prefixes() {
        assert_eq!(StableId::parse('a', "a42").unwrap().as_str(), "a42");
        assert!(StableId::parse('a', "c42").is_none());
        assert!(StableId::parse('a', "a").is_none());
        assert!(StableId::parse('a', "abc").is_none());
    }

    /// Verifies id factory can continue after restored ids.
    ///
    /// This regression scenario documents the behavior being protected so a
    /// failure points at a concrete contract change rather than an incidental
    /// implementation detail.
    #[test]
    fn id_factory_can_continue_after_restored_ids() {
        let ids = vec![
            StableId::parse('$', "$4").unwrap(),
            StableId::parse('g', "g3").unwrap(),
            StableId::parse('@', "@7").unwrap(),
            StableId::parse('%', "%9").unwrap(),
            StableId::parse('c', "c2").unwrap(),
        ];

        let mut factory = IdFactory::after_existing_ids(&ids);

        assert_eq!(factory.session().as_str(), "$5");
        assert_eq!(factory.window_group().as_str(), "g4");
        assert_eq!(factory.window().as_str(), "@8");
        assert_eq!(factory.pane().as_str(), "%10");
        assert_eq!(factory.client().as_str(), "c3");
        assert_eq!(factory.observer_request().as_str(), "o1");
        assert_eq!(factory.agent().as_str(), "a1");
    }
}
