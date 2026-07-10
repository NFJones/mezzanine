//! Agent tests for openai provider behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

mod catalog_auth;
mod compatible_chat;
mod response_errors;
mod response_parsing;
mod transport;
