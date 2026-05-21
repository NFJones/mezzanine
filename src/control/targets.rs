//! Control Targets implementation.
//!
//! This module owns the control targets boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{MezError, Result, Session, SplitDirection, Window};

// Strict target parsing and resolution.

/// Runs the window target checked resolved operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn window_target_checked_resolved(
    session: &Session,
    params: &str,
) -> Result<Option<String>> {
    let value = parse_json_object_value(params, "control params")?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("control params must be an object"))?;
    let mut candidates = Vec::new();

    if object.contains_key("target")
        && (object.contains_key("window_id")
            || object.contains_key("window_name")
            || object.contains_key("window_index"))
    {
        return Err(MezError::invalid_args(
            "WindowTarget contains multiple independent selectors",
        ));
    }
    if let Some(target) = object.get("target")
        && let Some(window_id) = resolve_window_target_value(session, target)?
    {
        candidates.push(window_id);
    }
    if let Some(window_id) = string_member(object, "window_id") {
        candidates.push(resolve_window_id(session, window_id)?);
    }
    if let Some(window_name) = string_member(object, "window_name") {
        candidates.push(resolve_window_selector(
            session,
            WindowTargetSelector::Name(window_name.to_string()),
        )?);
    }
    if let Some(window_index) = index_member(object, "window_index", "WindowTarget window_index")? {
        candidates.push(resolve_window_selector(
            session,
            WindowTargetSelector::Index(window_index),
        )?);
    }

    resolve_exclusive_target("WindowTarget", candidates)
}

/// Runs the pane target checked resolved operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn pane_target_checked_resolved(
    session: &Session,
    params: &str,
) -> Result<Option<String>> {
    let value = parse_json_object_value(params, "control params")?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("control params must be an object"))?;
    let mut candidates = Vec::new();

    if object.contains_key("target")
        && (object.contains_key("pane_id")
            || object.contains_key("pane_title")
            || object.contains_key("pane_index"))
    {
        return Err(MezError::invalid_args(
            "PaneTarget contains multiple independent selectors",
        ));
    }
    if let Some(target) = object.get("target")
        && let Some(pane_id) = resolve_pane_target_value(session, target)?
    {
        candidates.push(pane_id);
    }
    if let Some(pane_id) = string_member(object, "pane_id") {
        candidates.push(resolve_pane_id(session, pane_id)?);
    }
    if let Some(pane_title) = string_member(object, "pane_title") {
        let window = session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        candidates.push(resolve_pane_selector_in_window(
            window,
            PaneTargetSelector::Title(pane_title.to_string()),
        )?);
    }
    if let Some(pane_index) = index_member(object, "pane_index", "PaneTarget pane_index")? {
        let window = session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        candidates.push(resolve_pane_selector_in_window(
            window,
            PaneTargetSelector::Index(pane_index),
        )?);
    }

    resolve_exclusive_target("PaneTarget", candidates)
}

/// Runs the source pane target checked resolved operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn source_pane_target_checked_resolved(
    session: &Session,
    params: &str,
) -> Result<Option<String>> {
    let value = parse_json_object_value(params, "control params")?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("control params must be an object"))?;
    let mut candidates = Vec::new();

    if let Some(source) = object.get("source") {
        if let Some(source_text) = source.as_str() {
            candidates.push(resolve_pane_id(session, source_text)?);
        } else if let Some(pane_id) = resolve_pane_target_value(session, source)? {
            candidates.push(pane_id);
        }
    }
    if let Some(pane_id) = string_member(object, "source_pane_id") {
        candidates.push(resolve_pane_id(session, pane_id)?);
    }
    if !object.contains_key("source")
        && !object.contains_key("source_pane_id")
        && let Some(pane_id) = pane_target_checked_resolved(session, params)?
    {
        candidates.push(pane_id);
    }

    resolve_exclusive_target("PaneTarget source", candidates)
}

/// Runs the destination target checked resolved operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn destination_target_checked_resolved(
    session: &Session,
    params: &str,
) -> Result<Option<String>> {
    let value = parse_json_object_value(params, "control params")?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("control params must be an object"))?;
    let mut candidates = Vec::new();

    if let Some(destination) = object.get("destination") {
        if let Some(destination_text) = destination.as_str() {
            candidates.push(destination_text.to_string());
        } else if target_value_has_pane_shape(destination)
            && let Some(pane_id) = resolve_pane_target_value(session, destination)?
        {
            candidates.push(pane_id);
        } else if let Some(window_id) = resolve_window_target_value(session, destination)? {
            candidates.push(window_id);
        }
    }
    if let Some(pane_id) = string_member(object, "destination_pane_id") {
        candidates.push(resolve_pane_id(session, pane_id)?);
    }
    if let Some(window_id) = string_member(object, "destination_window_id") {
        candidates.push(resolve_window_id(session, window_id)?);
    }
    if let Some(destination) = string_member(object, "destination") {
        candidates.push(destination.to_string());
    }

    resolve_exclusive_target("destination target", candidates)
}

/// Runs the target value has pane shape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn target_value_has_pane_shape(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("pane_id")
        || object.contains_key("pane_index")
        || object.contains_key("pane_title")
        || object.contains_key("title")
        || object.contains_key("window")
        || object.contains_key("session")
        || (object.contains_key("window_id")
            && (object.contains_key("active")
                || object.contains_key("pane_index")
                || object.contains_key("pane_title")
                || object.contains_key("title")))
}

/// Carries Window Target Selector state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WindowTargetSelector {
    /// Represents the Index case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Index(usize),
    /// Represents the Name case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Name(String),
    /// Represents the Active case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Active,
}

/// Carries Pane Target Selector state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaneTargetSelector {
    /// Represents the Index case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Index(usize),
    /// Represents the Title case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Title(String),
    /// Represents the Active case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Active,
}

/// Runs the parse json object value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_json_object_value(body: &str, label: &str) -> Result<serde_json::Value> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MezError::invalid_args(format!("{label} must be valid JSON")))?;
    if !value.is_object() {
        return Err(MezError::invalid_args(format!("{label} must be an object")));
    }
    Ok(value)
}

/// Runs the resolve window target value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_window_target_value(
    session: &Session,
    value: &serde_json::Value,
) -> Result<Option<String>> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("WindowTarget must be an object"))?;
    let mut candidates = Vec::new();

    if let Some(window_id) = string_member(object, "window_id") {
        candidates.push(resolve_window_id(session, window_id)?);
    }

    let selector = window_selector_from_object(object)?;
    if let Some(session_id) = string_member(object, "session_id") {
        require_session_id_matches(session, session_id)?;
        candidates.push(resolve_required_window_selector(session, selector.clone())?);
    }
    if let Some(session_target) = object.get("session") {
        require_session_target_matches_value(session, session_target)?;
        candidates.push(resolve_required_window_selector(session, selector.clone())?);
    }
    if bool_true_member(object, "default_session")? {
        candidates.push(resolve_required_window_selector(session, selector.clone())?);
    }
    let has_scoped_window_alternative = object.contains_key("session_id")
        || object.contains_key("session")
        || object.contains_key("default_session");
    if !has_scoped_window_alternative && let Some(selector) = selector {
        candidates.push(resolve_window_selector(session, selector)?);
    }

    resolve_strict_single_target("WindowTarget", candidates)
}

/// Runs the resolve pane target value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_pane_target_value(
    session: &Session,
    value: &serde_json::Value,
) -> Result<Option<String>> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("PaneTarget must be an object"))?;
    let mut candidates = Vec::new();

    if let Some(pane_id) = string_member(object, "pane_id") {
        candidates.push(resolve_pane_id(session, pane_id)?);
    }

    let pane_selector = pane_selector_from_object(object)?;
    if let Some(window_id) = string_member(object, "window_id") {
        let window = window_by_id(session, &resolve_window_id(session, window_id)?)?;
        candidates.push(resolve_required_pane_selector(
            window,
            pane_selector.clone(),
        )?);
    }
    if let Some(window_target) = object.get("window") {
        let window_id = resolve_window_target_value(session, window_target)?
            .ok_or_else(|| MezError::invalid_args("PaneTarget window requires a WindowTarget"))?;
        let window = window_by_id(session, &window_id)?;
        candidates.push(resolve_required_pane_selector(
            window,
            pane_selector.clone(),
        )?);
    }
    if let Some(session_target) = object.get("session") {
        require_session_target_matches_value(session, session_target)?;
        match pane_selector.clone() {
            Some(PaneTargetSelector::Active) => candidates.push(active_pane_id(session)?),
            Some(_) => {
                return Err(MezError::invalid_args(
                    "PaneTarget session alternative supports only active=true",
                ));
            }
            None => {
                return Err(MezError::invalid_args(
                    "PaneTarget session alternative requires active=true",
                ));
            }
        }
    }
    let has_window_or_session_alternative = object.contains_key("window_id")
        || object.contains_key("window")
        || object.contains_key("session");
    if !has_window_or_session_alternative && let Some(selector) = pane_selector {
        let window = session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        candidates.push(resolve_pane_selector_in_window(window, selector)?);
    }

    resolve_strict_single_target("PaneTarget", candidates)
}

/// Runs the require session target matches value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn require_session_target_matches_value(
    session: &Session,
    value: &serde_json::Value,
) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("SessionTarget must be an object"))?;
    let mut matched = 0usize;
    if let Some(session_id) = string_member(object, "session_id") {
        matched += 1;
        require_session_id_matches(session, session_id)?;
    }
    if let Some(name) = string_member(object, "name") {
        matched += 1;
        if session.name != name {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "session target not found",
            ));
        }
    }
    if bool_true_member(object, "default")? {
        matched += 1;
    }
    match matched {
        1 => Ok(()),
        _ => Err(MezError::invalid_args(
            "SessionTarget must use exactly one of session_id, name, or default=true",
        )),
    }
}

/// Runs the require session id matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn require_session_id_matches(session: &Session, session_id: &str) -> Result<()> {
    if session.id.as_str() == session_id {
        Ok(())
    } else {
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "session target not found",
        ))
    }
}

/// Runs the window selector from object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_selector_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<WindowTargetSelector>> {
    let mut selectors = Vec::new();
    if let Some(index) = index_member(object, "window_index", "WindowTarget window_index")? {
        selectors.push(WindowTargetSelector::Index(index));
    }
    if let Some(index) = index_member(object, "index", "WindowTarget index")? {
        selectors.push(WindowTargetSelector::Index(index));
    }
    if let Some(name) = string_member(object, "window_name") {
        selectors.push(WindowTargetSelector::Name(name.to_string()));
    }
    if let Some(name) = string_member(object, "name") {
        selectors.push(WindowTargetSelector::Name(name.to_string()));
    }
    if bool_true_member(object, "active")? {
        selectors.push(WindowTargetSelector::Active);
    }
    resolve_strict_single_selector("WindowTarget window selector", selectors)
}

/// Runs the pane selector from object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_selector_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<PaneTargetSelector>> {
    let mut selectors = Vec::new();
    if let Some(index) = index_member(object, "pane_index", "PaneTarget pane_index")? {
        selectors.push(PaneTargetSelector::Index(index));
    }
    if let Some(index) = index_member(object, "index", "PaneTarget index")? {
        selectors.push(PaneTargetSelector::Index(index));
    }
    if let Some(title) = string_member(object, "pane_title") {
        selectors.push(PaneTargetSelector::Title(title.to_string()));
    }
    if let Some(title) = string_member(object, "title") {
        selectors.push(PaneTargetSelector::Title(title.to_string()));
    }
    if bool_true_member(object, "active")? {
        selectors.push(PaneTargetSelector::Active);
    }
    resolve_strict_single_selector("PaneTarget pane selector", selectors)
}

/// Runs the resolve required window selector operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_required_window_selector(
    session: &Session,
    selector: Option<WindowTargetSelector>,
) -> Result<String> {
    let selector = selector.ok_or_else(|| {
        MezError::invalid_args("WindowTarget session alternative requires a window selector")
    })?;
    resolve_window_selector(session, selector)
}

/// Runs the resolve window selector operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_window_selector(
    session: &Session,
    selector: WindowTargetSelector,
) -> Result<String> {
    match selector {
        WindowTargetSelector::Index(index) => session
            .windows()
            .iter()
            .find(|window| window.index == index)
            .map(|window| window.id.to_string())
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found")),
        WindowTargetSelector::Name(name) => session
            .windows()
            .iter()
            .find(|window| window.name == name)
            .map(|window| window.id.to_string())
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found")),
        WindowTargetSelector::Active => session
            .active_window()
            .map(|window| window.id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active window")),
    }
}

/// Runs the resolve window id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_window_id(session: &Session, window_id: &str) -> Result<String> {
    session
        .windows()
        .iter()
        .find(|window| window.id.as_str() == window_id)
        .map(|window| window.id.to_string())
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found"))
}

/// Runs the resolve required pane selector operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_required_pane_selector(
    window: &Window,
    selector: Option<PaneTargetSelector>,
) -> Result<String> {
    let selector = selector.ok_or_else(|| {
        MezError::invalid_args("PaneTarget window alternative requires a pane selector")
    })?;
    resolve_pane_selector_in_window(window, selector)
}

/// Runs the resolve pane selector in window operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_pane_selector_in_window(
    window: &Window,
    selector: PaneTargetSelector,
) -> Result<String> {
    match selector {
        PaneTargetSelector::Index(index) => window
            .panes()
            .iter()
            .find(|pane| pane.index == index)
            .map(|pane| pane.id.to_string())
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")),
        PaneTargetSelector::Title(title) => window
            .panes()
            .iter()
            .find(|pane| pane.title == title)
            .map(|pane| pane.id.to_string())
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")),
        PaneTargetSelector::Active => Ok(window.active_pane().id.to_string()),
    }
}

/// Runs the resolve pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_pane_id(session: &Session, pane_id: &str) -> Result<String> {
    pane_by_id(session, pane_id).map(|(_, pane)| pane.id.to_string())
}

/// Runs the active pane id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn active_pane_id(session: &Session) -> Result<String> {
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    Ok(window.active_pane().id.to_string())
}

/// Runs the string member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn string_member<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    object.get(key).and_then(serde_json::Value::as_str)
}

/// Runs the bool true member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn bool_true_member(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<bool> {
    match object.get(key) {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| MezError::invalid_args(format!("{key} must be a boolean"))),
        None => Ok(false),
    }
}

/// Runs the index member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn index_member(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
) -> Result<Option<usize>> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let index = value
        .as_u64()
        .ok_or_else(|| MezError::invalid_args(format!("{label} must be a non-negative integer")))?;
    usize::try_from(index)
        .map(Some)
        .map_err(|_| MezError::invalid_args(format!("{label} is out of range")))
}

/// Runs the resolve strict single selector operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_strict_single_selector<T>(
    label: &str,
    selectors: Vec<T>,
) -> Result<Option<T>> {
    let mut selectors = selectors.into_iter();
    let Some(first) = selectors.next() else {
        return Ok(None);
    };
    if selectors.next().is_some() {
        return Err(MezError::invalid_args(format!(
            "{label} contains multiple independent selectors"
        )));
    }
    Ok(Some(first))
}

/// Runs the resolve strict single target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_strict_single_target(
    label: &str,
    candidates: Vec<String>,
) -> Result<Option<String>> {
    let mut candidates = candidates.into_iter();
    let Some(first) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Err(MezError::invalid_args(format!(
            "{label} contains multiple independent selectors"
        )));
    }
    Ok(Some(first))
}

/// Runs the resolve exclusive target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn resolve_exclusive_target(
    label: &str,
    candidates: Vec<String>,
) -> Result<Option<String>> {
    let Some(first) = candidates.first().cloned() else {
        return Ok(None);
    };
    if candidates.iter().all(|candidate| candidate == &first) {
        return Ok(Some(first));
    }
    Err(MezError::invalid_args(format!(
        "{label} contains multiple independent selectors"
    )))
}

/// Runs the parse split direction operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_split_direction(value: &str) -> Result<SplitDirection> {
    match value {
        "vertical" | "right" | "left" => Ok(SplitDirection::Vertical),
        "horizontal" | "above" | "below" | "up" | "down" => Ok(SplitDirection::Horizontal),
        _ => Err(MezError::invalid_args("unsupported pane split direction")),
    }
}

/// Runs the parse join position operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_join_position(value: &str) -> Result<SplitDirection> {
    parse_split_direction(value)
}

/// Runs the window by id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_by_id<'a>(session: &'a Session, window_id: &str) -> Result<&'a Window> {
    session
        .windows()
        .iter()
        .find(|window| window.id.as_str() == window_id)
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found"))
}

/// Runs the window id for target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn window_id_for_target(session: &Session, target: Option<&str>) -> Result<String> {
    let Some(target) = target else {
        return session
            .active_window()
            .map(|window| window.id.to_string())
            .ok_or_else(|| MezError::invalid_state("session has no active window"));
    };
    session
        .windows()
        .iter()
        .find(|window| window.id.as_str() == target)
        .or_else(|| {
            session
                .windows()
                .iter()
                .find(|window| window.index.to_string() == target)
        })
        .or_else(|| {
            session
                .windows()
                .iter()
                .find(|window| window.name == target)
        })
        .map(|window| window.id.to_string())
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "window not found"))
}

/// Runs the pane by id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_by_id<'a>(
    session: &'a Session,
    pane_id: &str,
) -> Result<(&'a Window, &'a crate::layout::Pane)> {
    session
        .windows()
        .iter()
        .find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| (window, pane))
        })
        .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found"))
}

/// Runs the target or active pane operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn target_or_active_pane<'a>(
    session: &'a Session,
    target: Option<&str>,
) -> Result<(&'a Window, &'a crate::layout::Pane)> {
    if let Some(target) = target {
        return pane_by_id(session, target);
    }
    let window = session
        .active_window()
        .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
    Ok((window, window.active_pane()))
}
