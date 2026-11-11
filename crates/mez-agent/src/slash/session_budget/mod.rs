//! Shared exact budget grammar, deliberately not yet in the runtime registry.
//! Parsing performs no mutation, membership lookup, persistence or admission.

use crate::token_budget::{BudgetOverride, TokenLimit};

/// Syntax shared by eventual human command/help/control ingress.
pub const SESSION_BUDGET_USAGE: &str = "usage: /session-budget [status | [--global] --input VALUE [--output VALUE] | [--global] --output VALUE [--input VALUE]]; VALUE = NUMBER | unlimited | inherit";

/// Complete validated command. Both dimensions parse before any caller effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionBudgetCommand {
    /// Bare command or exact status, always read-only.
    Status,
    /// At least one field changes; omitted fields remain unchanged.
    Mutate(SessionBudgetMutation),
}

/// All flags may appear in any order once; scope does not change value grammar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionBudgetMutation {
    /// Primary config instead of invoking conversation's immutable root.
    pub global: bool,
    /// None = unchanged; Inherit = remove override/key; explicit = install value.
    pub input: Option<BudgetOverride>,
    /// Independent output field with the same three-way mutation semantics.
    pub output: Option<BudgetOverride>,
}

/// Tokenizes once with shlex. Quoted values stay single operands; no shell runs.
pub fn parse_session_budget_options(args: &str) -> Result<SessionBudgetCommand, &'static str> {
    let words = shlex::split(args).ok_or(SESSION_BUDGET_USAGE)?;
    parse_session_budget_option_tokens(words.iter().map(String::as_str))
}

/// Validates already-tokenized CLI/control operands without joining/re-splitting.
pub fn parse_session_budget_option_tokens<'a>(
    args: impl IntoIterator<Item = &'a str>,
) -> Result<SessionBudgetCommand, &'static str> {
    let mut words = args.into_iter().peekable();
    if words.peek().is_none() {
        return Ok(SessionBudgetCommand::Status);
    }
    if words.peek() == Some(&"status") {
        words.next();
        return if words.next().is_none() {
            Ok(SessionBudgetCommand::Status)
        } else {
            Err(SESSION_BUDGET_USAGE)
        };
    }
    let mut result = SessionBudgetMutation::default();
    while let Some(flag) = words.next() {
        match flag {
            "--global" if !result.global => result.global = true,
            "--input" | "--output" => {
                let field = if flag == "--input" {
                    &mut result.input
                } else {
                    &mut result.output
                };
                if field.is_some() {
                    return Err(SESSION_BUDGET_USAGE);
                }
                let value = words.next().ok_or(SESSION_BUDGET_USAGE)?;
                *field = Some(if value == "inherit" {
                    BudgetOverride::Inherit
                } else {
                    BudgetOverride::Explicit(
                        TokenLimit::parse(value).map_err(|_| SESSION_BUDGET_USAGE)?,
                    )
                });
            }
            _ => return Err(SESSION_BUDGET_USAGE),
        }
    }
    if result.input.is_none() && result.output.is_none() {
        return Err(SESSION_BUDGET_USAGE);
    }
    Ok(SessionBudgetCommand::Mutate(result))
}

#[cfg(test)]
mod tests;
