//! Shared subagent request and scope coordination types.
//!
//! These types represent the public contract for spawning helper agents and
//! tracking write ownership across concurrent work.

use std::collections::BTreeMap;

pub use mez_agent::{CooperationMode, SubagentScopeDeclaration};

use crate::permissions::PermissionPreset;

/// Built-in human-readable subagent display names.
///
/// The list deliberately uses short, familiar first names so subagent panes and
/// parent status lines stay compact while remaining easier to distinguish than
/// canonical runtime ids such as `agent-%2`.
pub const SUBAGENT_FRIENDLY_NAMES: &[&str] = &[
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

/// Built-in subagent roles understood by the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinSubagentRole {
    /// General-purpose role.
    Default,
    /// Write-capable implementation role.
    Worker,
    /// Read-only exploration role.
    Explorer,
}

/// Request to create a child agent with requested task-scope metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSpawnRequest {
    /// Parent agent requesting the spawn.
    pub parent_agent_id: String,
    /// Role or configured custom profile requested for the child.
    pub requested_role: String,
    /// Placement hint for the new pane or execution surface.
    pub placement: String,
    /// Cooperation model for write access.
    pub cooperation_mode: CooperationMode,
    /// Whether cooperation mode came from a profile/default placeholder.
    pub cooperation_mode_defaulted: bool,
    /// Requested scopes the child may inspect; enforceable scopes are inherited.
    pub read_scopes: Vec<String>,
    /// Whether read scopes came from profile defaults.
    pub read_scopes_defaulted: bool,
    /// Requested scopes the child may mutate; enforceable scopes are inherited.
    pub write_scopes: Vec<String>,
    /// Whether write scopes came from profile defaults.
    pub write_scopes_defaulted: bool,
    /// Initial task prompt for the child agent.
    pub task_prompt: String,
    /// When true the runtime creates the child pane and enters agent mode
    /// but does not start an initial provider turn. Macro orchestration
    /// uses this to create an idle persistent child session whose first
    /// real turn is delivered through the macro step bridge.
    pub skip_initial_turn: bool,
    /// Whether the user explicitly approved unrestricted writes.
    pub explicit_user_approval: bool,
}

/// Configured subagent profile metadata and defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentProfile {
    /// Stable profile identity used by spawn requests.
    pub id: String,
    /// User-visible display name.
    pub name: String,
    /// User-visible profile description.
    pub description: String,
    /// Developer instructions appended to the child task prompt.
    pub developer_instructions: Option<String>,
    /// Optional model-profile override for this child role.
    pub model_profile: Option<String>,
    /// Optional stricter permission preset override name.
    pub permission_preset: Option<PermissionPreset>,
    /// MCP servers the child should prefer or restrict to.
    pub mcp_servers: Vec<String>,
    /// Extra shell environment entries requested by the profile.
    pub shell_env: BTreeMap<String, String>,
    /// Default cooperation mode when callers do not provide one explicitly.
    pub default_cooperation_mode: Option<CooperationMode>,
    /// Default requested read scopes merged into spawn requests.
    pub default_read_scopes: Vec<String>,
    /// Default requested write scopes merged into spawn requests.
    pub default_write_scopes: Vec<String>,
}

/// Registered active write ownership for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWriteScope {
    /// Agent holding the scope registration.
    pub agent_id: String,
    /// Cooperation mode used for this registration.
    pub mode: CooperationMode,
    /// Normalized path-like write scope.
    pub scope: String,
    /// Optional serial lock used by serial-write registrations.
    pub serial_lock: Option<String>,
}

/// Describes a requested write scope that overlaps an active registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeConflict {
    /// Agent that currently holds the overlapping scope.
    pub existing_agent_id: String,
    /// Active normalized scope that caused the conflict.
    pub existing_scope: String,
    /// Requested normalized scope that conflicted.
    pub requested_scope: String,
}

/// Registry of active write-scope ownership by agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRegistry {
    /// Stores the active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) active: BTreeMap<String, Vec<ActiveWriteScope>>,
}
