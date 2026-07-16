//! Runtime agent provider execution and completion helpers.
//!
//! This module owns provider turn execution, provider completion ingress,
//! assistant/progress context insertion, and execution settlement after a
//! model response. The surrounding runtime agent facade still owns shared
//! session state, while this module keeps provider-response control flow in
//! one focused implementation unit.

mod completion;
mod context;
mod loop_control;
mod result_apply;
mod result_apply_async;
mod worker;
