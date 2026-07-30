//! Typed sandbox toolchain discovery and projection metadata.
//!
//! This module is the single owner for allowlisted toolchain names, fixed
//! in-sandbox projection paths, and validation of host roots supplied either
//! by pane bootstrap evidence or the direct-user CLI adapter. Runtime
//! discovery never consults ambient process state, and discovery alone never
//! grants filesystem authority; final launch compilation still checks the
//! validated roots against pane-resolved maximum read authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::{
    BubblewrapConfig, CustomToolchainDefinition, CustomToolchainReference, SandboxToolchainKind,
    ToolchainSelection,
};
use mez_agent::permissions::PathScopes;
use sha2::{Digest, Sha256};

use super::{
    SandboxCompileError, SandboxCompileErrorKind, path_overlaps, validate_printable_absolute_path,
};

/// Stable supported toolchain kinds in display and completion order.
pub(crate) const SUPPORTED_SANDBOX_TOOLCHAIN_KINDS: [SandboxToolchainKind; 28] = [
    SandboxToolchainKind::Rust,
    SandboxToolchainKind::Zig,
    SandboxToolchainKind::Go,
    SandboxToolchainKind::Deno,
    SandboxToolchainKind::Bun,
    SandboxToolchainKind::Node,
    SandboxToolchainKind::Python,
    SandboxToolchainKind::Jdk,
    SandboxToolchainKind::Maven,
    SandboxToolchainKind::Gradle,
    SandboxToolchainKind::Dotnet,
    SandboxToolchainKind::Dart,
    SandboxToolchainKind::Kotlin,
    SandboxToolchainKind::Ruby,
    SandboxToolchainKind::Php,
    SandboxToolchainKind::Composer,
    SandboxToolchainKind::Erlang,
    SandboxToolchainKind::Elixir,
    SandboxToolchainKind::Ghc,
    SandboxToolchainKind::Cabal,
    SandboxToolchainKind::Stack,
    SandboxToolchainKind::Ocaml,
    SandboxToolchainKind::Llvm,
    SandboxToolchainKind::Gcc,
    SandboxToolchainKind::Cmake,
    SandboxToolchainKind::Ninja,
    SandboxToolchainKind::Meson,
    SandboxToolchainKind::Swift,
];

/// Fixed Cargo executable projection inside Bubblewrap.
pub(crate) const SANDBOX_RUST_CARGO_BIN: &str = "/opt/mez/toolchains/rust/cargo-bin";
/// Fixed Rustup home projection inside Bubblewrap.
pub(crate) const SANDBOX_RUSTUP_HOME: &str = "/opt/mez/toolchains/rust/rustup";
/// Fixed executable search path used when Rust is projected.
pub(crate) const SANDBOX_RUST_PATH: &str = "/opt/mez/toolchains/rust/cargo-bin:/usr/bin:/bin";
/// Fixed Zig distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_ZIG_ROOT: &str = "/opt/mez/toolchains/zig";
/// Fixed executable search path used when only Zig is projected.
pub(crate) const SANDBOX_ZIG_PATH: &str = "/opt/mez/toolchains/zig:/usr/bin:/bin";
/// Fixed Go SDK projection inside Bubblewrap.
pub(crate) const SANDBOX_GO_ROOT: &str = "/opt/mez/toolchains/go/root";
/// Fixed executable search path used when only Go is projected.
pub(crate) const SANDBOX_GO_PATH: &str = "/opt/mez/toolchains/go/root/bin:/usr/bin:/bin";
/// Fixed Deno runtime projection inside Bubblewrap.
pub(crate) const SANDBOX_DENO_ROOT: &str = "/opt/mez/toolchains/deno";
/// Fixed executable search path used when only Deno is projected.
pub(crate) const SANDBOX_DENO_PATH: &str = "/opt/mez/toolchains/deno:/usr/bin:/bin";
/// Fixed Bun distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_BUN_ROOT: &str = "/opt/mez/toolchains/bun/root";
/// Fixed executable search path used when only Bun is projected.
pub(crate) const SANDBOX_BUN_PATH: &str = "/opt/mez/toolchains/bun/root/bin:/usr/bin:/bin";
/// Fixed Node.js distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_NODE_ROOT: &str = "/opt/mez/toolchains/node/root";
/// Fixed executable search path used when only Node.js is projected.
pub(crate) const SANDBOX_NODE_PATH: &str = "/opt/mez/toolchains/node/root/bin:/usr/bin:/bin";
/// Fixed Python base-runtime projection inside Bubblewrap.
pub(crate) const SANDBOX_PYTHON_ROOT: &str = "/opt/mez/toolchains/python/root";
/// Fixed executable search path used when only the Python base runtime is projected.
pub(crate) const SANDBOX_PYTHON_PATH: &str = "/opt/mez/toolchains/python/root/bin:/usr/bin:/bin";
/// Fixed Java Development Kit projection inside Bubblewrap.
pub(crate) const SANDBOX_JDK_ROOT: &str = "/opt/mez/toolchains/jdk/root";
/// Fixed executable search path used when only the JDK is projected.
pub(crate) const SANDBOX_JDK_PATH: &str = "/opt/mez/toolchains/jdk/root/bin:/usr/bin:/bin";
/// Fixed Maven distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_MAVEN_ROOT: &str = "/opt/mez/toolchains/maven/root";
/// Fixed executable search path when Maven is composed with its required JDK.
pub(crate) const SANDBOX_JDK_MAVEN_PATH: &str =
    "/opt/mez/toolchains/jdk/root/bin:/opt/mez/toolchains/maven/root/bin:/usr/bin:/bin";
/// Fixed Gradle distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_GRADLE_ROOT: &str = "/opt/mez/toolchains/gradle/root";
/// Fixed executable search path when Gradle is composed with its required JDK.
pub(crate) const SANDBOX_JDK_GRADLE_PATH: &str =
    "/opt/mez/toolchains/jdk/root/bin:/opt/mez/toolchains/gradle/root/bin:/usr/bin:/bin";
/// Fixed .NET SDK projection inside Bubblewrap.
pub(crate) const SANDBOX_DOTNET_ROOT: &str = "/opt/mez/toolchains/dotnet/root";
/// Fixed executable search path used when only the .NET SDK is projected.
pub(crate) const SANDBOX_DOTNET_PATH: &str = "/opt/mez/toolchains/dotnet/root:/usr/bin:/bin";
/// Fixed Dart SDK projection inside Bubblewrap.
pub(crate) const SANDBOX_DART_ROOT: &str = "/opt/mez/toolchains/dart/root";
/// Fixed executable search path used when only the Dart SDK is projected.
pub(crate) const SANDBOX_DART_PATH: &str = "/opt/mez/toolchains/dart/root/bin:/usr/bin:/bin";
/// Fixed Kotlin/JVM compiler projection inside Bubblewrap.
pub(crate) const SANDBOX_KOTLIN_ROOT: &str = "/opt/mez/toolchains/kotlin/root";
/// Fixed executable search path when Kotlin/JVM is composed with its required JDK.
pub(crate) const SANDBOX_KOTLIN_JDK_PATH: &str =
    "/opt/mez/toolchains/jdk/root/bin:/opt/mez/toolchains/kotlin/root/bin:/usr/bin:/bin";
/// Fixed Ruby runtime projection inside Bubblewrap.
pub(crate) const SANDBOX_RUBY_ROOT: &str = "/opt/mez/toolchains/ruby/root";
/// Fixed executable search path used when Ruby is projected.
pub(crate) const SANDBOX_RUBY_PATH: &str = "/opt/mez/toolchains/ruby/root/bin:/usr/bin:/bin";
/// Fixed PHP runtime projection inside Bubblewrap.
pub(crate) const SANDBOX_PHP_ROOT: &str = "/opt/mez/toolchains/php/root";
/// Fixed executable search path used when only PHP is projected.
pub(crate) const SANDBOX_PHP_PATH: &str = "/opt/mez/toolchains/php/root/bin:/usr/bin:/bin";
/// Fixed Composer companion projection inside Bubblewrap.
pub(crate) const SANDBOX_COMPOSER_ROOT: &str = "/opt/mez/toolchains/composer/root";
/// Fixed executable search path when Composer is composed with PHP.
pub(crate) const SANDBOX_PHP_COMPOSER_PATH: &str =
    "/opt/mez/toolchains/php/root/bin:/opt/mez/toolchains/composer/root/bin:/usr/bin:/bin";
/// Fixed Erlang/OTP runtime projection inside Bubblewrap.
pub(crate) const SANDBOX_ERLANG_ROOT: &str = "/opt/mez/toolchains/erlang/root";
/// Fixed executable search path used when only Erlang/OTP is projected.
pub(crate) const SANDBOX_ERLANG_PATH: &str = "/opt/mez/toolchains/erlang/root/bin:/usr/bin:/bin";
/// Fixed Elixir compiler and Mix projection inside Bubblewrap.
pub(crate) const SANDBOX_ELIXIR_ROOT: &str = "/opt/mez/toolchains/elixir/root";
/// Fixed executable search path when Elixir is composed with Erlang/OTP.
pub(crate) const SANDBOX_ERLANG_ELIXIR_PATH: &str =
    "/opt/mez/toolchains/erlang/root/bin:/opt/mez/toolchains/elixir/root/bin:/usr/bin:/bin";
/// Fixed GHC compiler projection inside Bubblewrap.
pub(crate) const SANDBOX_GHC_ROOT: &str = "/opt/mez/toolchains/ghc/root";
/// Fixed executable search path used when only GHC is projected.
pub(crate) const SANDBOX_GHC_PATH: &str = "/opt/mez/toolchains/ghc/root/bin:/usr/bin:/bin";
/// Fixed Cabal companion projection inside Bubblewrap.
pub(crate) const SANDBOX_CABAL_ROOT: &str = "/opt/mez/toolchains/cabal/root";
/// Fixed Stack companion projection inside Bubblewrap.
pub(crate) const SANDBOX_STACK_ROOT: &str = "/opt/mez/toolchains/stack/root";
/// Fixed executable search path when Cabal is composed with GHC.
pub(crate) const SANDBOX_GHC_CABAL_PATH: &str =
    "/opt/mez/toolchains/ghc/root/bin:/opt/mez/toolchains/cabal/root/bin:/usr/bin:/bin";
/// Fixed executable search path when Stack is composed with GHC.
pub(crate) const SANDBOX_GHC_STACK_PATH: &str =
    "/opt/mez/toolchains/ghc/root/bin:/opt/mez/toolchains/stack/root/bin:/usr/bin:/bin";
/// Fixed executable search path when both Haskell companions are selected.
pub(crate) const SANDBOX_GHC_CABAL_STACK_PATH: &str = "/opt/mez/toolchains/ghc/root/bin:/opt/mez/toolchains/cabal/root/bin:/opt/mez/toolchains/stack/root/bin:/usr/bin:/bin";
/// Fixed LLVM/Clang toolchain projection inside Bubblewrap.
pub(crate) const SANDBOX_LLVM_ROOT: &str = "/opt/mez/toolchains/llvm/root";
/// Fixed executable search path used when only LLVM/Clang is projected.
pub(crate) const SANDBOX_LLVM_PATH: &str = "/opt/mez/toolchains/llvm/root/bin:/usr/bin:/bin";
/// Fixed GCC toolchain projection inside Bubblewrap.
pub(crate) const SANDBOX_GCC_ROOT: &str = "/opt/mez/toolchains/gcc/root";
/// Fixed executable search path used when only GCC is projected.
pub(crate) const SANDBOX_GCC_PATH: &str = "/opt/mez/toolchains/gcc/root/bin:/usr/bin:/bin";
/// Fixed CMake distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_CMAKE_ROOT: &str = "/opt/mez/toolchains/cmake/root";
/// Fixed executable search path used when only CMake is projected.
pub(crate) const SANDBOX_CMAKE_PATH: &str = "/opt/mez/toolchains/cmake/root/bin:/usr/bin:/bin";
/// Fixed Ninja distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_NINJA_ROOT: &str = "/opt/mez/toolchains/ninja/root";
/// Fixed executable search path used when only Ninja is projected.
pub(crate) const SANDBOX_NINJA_PATH: &str = "/opt/mez/toolchains/ninja/root/bin:/usr/bin:/bin";
/// Fixed Meson distribution projection inside Bubblewrap.
pub(crate) const SANDBOX_MESON_ROOT: &str = "/opt/mez/toolchains/meson/root";
/// Fixed executable search path used when only Meson is projected.
pub(crate) const SANDBOX_MESON_PATH: &str = "/opt/mez/toolchains/meson/root/bin:/usr/bin:/bin";
/// Fixed Swift toolchain projection inside Bubblewrap.
pub(crate) const SANDBOX_SWIFT_ROOT: &str = "/opt/mez/toolchains/swift/root";
/// Fixed executable search path used when Swift is projected on Linux.
pub(crate) const SANDBOX_SWIFT_PATH: &str = "/opt/mez/toolchains/swift/root/bin:/usr/bin:/bin";

/// Security class assigned to one descriptor-owned projection resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolchainAuthorityClass {
    /// Immutable runtime or SDK content projected read-only.
    Runtime,
    /// Repository-controlled executable state already covered by project authority.
    ProjectEnvironment,
    /// Separately selected user-installed executable content.
    UserTools,
    /// Writable state created only beneath the Mezzanine-managed home.
    ManagedState,
    /// Credential or user configuration state that remains hidden.
    Credential,
}

/// Host platform constraint declared by one toolchain descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolchainPlatform {
    /// Descriptor is portable across supported Bubblewrap host platforms.
    Any,
    /// Descriptor is supported only for Linux pane environments.
    Linux,
    /// Descriptor is supported only for macOS pane environments.
    MacOs,
    /// Descriptor is supported only for Windows pane environments.
    Windows,
}

impl ToolchainPlatform {
    /// Reports whether one normalized pane operating-system spelling is supported.
    pub(super) fn supports(self, host_os: &str) -> bool {
        self == Self::Any
            || matches!(
                (self, host_os),
                (Self::Linux, "linux")
                    | (Self::MacOs, "macos" | "darwin")
                    | (Self::Windows, "windows")
            )
    }
}

/// One fixed root expected from bounded pane-bootstrap evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainRootDescriptor {
    /// Stable environment-manager evidence record name.
    pub(crate) evidence_kind: &'static str,
    /// Human-readable label used in fail-closed diagnostics.
    pub(crate) label: &'static str,
    /// Fixed code-owned destination inside the sandbox.
    pub(crate) sandbox_destination: &'static str,
    /// Allowed final canonical path components.
    pub(crate) allowed_names: &'static [&'static str],
    /// Optional allowed parent components for narrow executable directories.
    pub(crate) allowed_parent_names: &'static [&'static str],
    /// Security class governing this root.
    pub(crate) authority_class: ToolchainAuthorityClass,
    /// Executables that must be real files directly beneath this root.
    pub(crate) required_executables: &'static [&'static str],
    /// Distribution directories that must be real directories beneath this root.
    pub(crate) required_directories: &'static [&'static str],
}

/// One synthesized child environment value owned by a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainEnvironmentVariable {
    /// Environment variable name.
    pub(crate) name: &'static str,
    /// Fixed sandbox-visible value.
    pub(crate) value: &'static str,
}

/// One writable state location created beneath the managed sandbox home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagedToolchainState {
    /// Stable state purpose used by status and future quota reporting.
    pub(crate) purpose: &'static str,
    /// Fixed sandbox path beneath `/home/mez`.
    pub(crate) sandbox_path: &'static str,
}

/// Required and optional companion kinds for a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainCoupling {
    /// Kinds that must also be selected.
    pub(crate) required: &'static [SandboxToolchainKind],
    /// Kinds that may be composed when selected explicitly.
    pub(crate) optional: &'static [SandboxToolchainKind],
}

/// Stable code-owned behavior for one allowlisted toolchain kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolchainDescriptor {
    /// Persisted typed kind.
    pub(crate) kind: SandboxToolchainKind,
    /// Accepted user-facing aliases.
    pub(crate) aliases: &'static [&'static str],
    /// Fixed roots resolved from bounded evidence.
    pub(crate) roots: &'static [ToolchainRootDescriptor],
    /// Parent directories created before fixed mounts.
    pub(crate) sandbox_directories: &'static [&'static str],
    /// Executable search paths in descriptor-owned priority order.
    pub(crate) path_entries: &'static [&'static str],
    /// Explicit child environment synthesized from sandbox paths.
    pub(crate) environment: &'static [ToolchainEnvironmentVariable],
    /// Writable state redirected beneath the managed home.
    pub(crate) managed_state: &'static [ManagedToolchainState],
    /// Host descendants that this descriptor never projects.
    pub(crate) forbidden_descendants: &'static [&'static str],
    /// Supported host platform.
    pub(crate) platform: ToolchainPlatform,
    /// Companion dependency contract.
    pub(crate) coupling: ToolchainCoupling,
    /// Whether explicitly modeled roots may contain or overlap one another.
    pub(crate) allow_root_overlap: bool,
}

const RUST_ROOTS: [ToolchainRootDescriptor; 2] = [
    ToolchainRootDescriptor {
        evidence_kind: "cargo-bin",
        label: "Cargo bin",
        sandbox_destination: SANDBOX_RUST_CARGO_BIN,
        allowed_names: &["bin"],
        allowed_parent_names: &[".cargo", "cargo"],
        authority_class: ToolchainAuthorityClass::UserTools,
        required_executables: &[],
        required_directories: &[],
    },
    ToolchainRootDescriptor {
        evidence_kind: "rustup",
        label: "Rustup home",
        sandbox_destination: SANDBOX_RUSTUP_HOME,
        allowed_names: &[".rustup", "rustup"],
        allowed_parent_names: &[],
        authority_class: ToolchainAuthorityClass::Runtime,
        required_executables: &[],
        required_directories: &[],
    },
];
const RUST_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "CARGO_HOME",
        value: "/home/mez/.cargo",
    },
    ToolchainEnvironmentVariable {
        name: "RUSTUP_HOME",
        value: SANDBOX_RUSTUP_HOME,
    },
];
const RUST_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "cargo-home",
    sandbox_path: "/home/mez/.cargo",
}];
const RUST_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Rust,
    aliases: &["rust"],
    roots: &RUST_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/rust",
    ],
    path_entries: &[SANDBOX_RUST_CARGO_BIN],
    environment: &RUST_ENVIRONMENT,
    managed_state: &RUST_MANAGED_STATE,
    forbidden_descendants: &["credentials", "credentials.toml", "config.toml"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const ZIG_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "zig",
    label: "Zig distribution",
    sandbox_destination: SANDBOX_ZIG_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["zig"],
    required_directories: &["lib"],
}];
const ZIG_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "ZIG_GLOBAL_CACHE_DIR",
    value: "/home/mez/.cache/zig",
}];
const ZIG_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "zig-global-cache",
    sandbox_path: "/home/mez/.cache/zig",
}];
const ZIG_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Zig,
    aliases: &["zig"],
    roots: &ZIG_ROOTS,
    sandbox_directories: &["/opt", "/opt/mez", "/opt/mez/toolchains"],
    path_entries: &[SANDBOX_ZIG_ROOT],
    environment: &ZIG_ENVIRONMENT,
    managed_state: &ZIG_MANAGED_STATE,
    forbidden_descendants: &["shims", "credentials", "config.toml"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const GO_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "go",
    label: "Go SDK",
    sandbox_destination: SANDBOX_GO_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/go"],
    required_directories: &["src"],
}];
const GO_ENVIRONMENT: [ToolchainEnvironmentVariable; 4] = [
    ToolchainEnvironmentVariable {
        name: "GOROOT",
        value: SANDBOX_GO_ROOT,
    },
    ToolchainEnvironmentVariable {
        name: "GOPATH",
        value: "/home/mez/go",
    },
    ToolchainEnvironmentVariable {
        name: "GOMODCACHE",
        value: "/home/mez/go/pkg/mod",
    },
    ToolchainEnvironmentVariable {
        name: "GOCACHE",
        value: "/home/mez/.cache/go-build",
    },
];
const GO_MANAGED_STATE: [ManagedToolchainState; 3] = [
    ManagedToolchainState {
        purpose: "go-workspace",
        sandbox_path: "/home/mez/go",
    },
    ManagedToolchainState {
        purpose: "go-module-cache",
        sandbox_path: "/home/mez/go/pkg/mod",
    },
    ManagedToolchainState {
        purpose: "go-build-cache",
        sandbox_path: "/home/mez/.cache/go-build",
    },
];
const GO_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Go,
    aliases: &["go", "golang"],
    roots: &GO_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/go",
    ],
    path_entries: &["/opt/mez/toolchains/go/root/bin"],
    environment: &GO_ENVIRONMENT,
    managed_state: &GO_MANAGED_STATE,
    forbidden_descendants: &["credentials", "config", "pkg/mod/cache/vcs"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const DENO_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "deno",
    label: "Deno runtime",
    sandbox_destination: SANDBOX_DENO_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["deno"],
    required_directories: &[],
}];
const DENO_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "DENO_DIR",
    value: "/home/mez/.cache/deno",
}];
const DENO_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "deno-cache",
    sandbox_path: "/home/mez/.cache/deno",
}];
const DENO_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Deno,
    aliases: &["deno"],
    roots: &DENO_ROOTS,
    sandbox_directories: &["/opt", "/opt/mez", "/opt/mez/toolchains"],
    path_entries: &[SANDBOX_DENO_ROOT],
    environment: &DENO_ENVIRONMENT,
    managed_state: &DENO_MANAGED_STATE,
    forbidden_descendants: &["auth_tokens", "credentials", "certificates", "bin"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const BUN_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "bun",
    label: "Bun distribution",
    sandbox_destination: SANDBOX_BUN_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/bun"],
    required_directories: &[],
}];
const BUN_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "BUN_INSTALL",
        value: SANDBOX_BUN_ROOT,
    },
    ToolchainEnvironmentVariable {
        name: "BUN_INSTALL_CACHE_DIR",
        value: "/home/mez/.cache/bun",
    },
];
const BUN_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "bun-package-cache",
    sandbox_path: "/home/mez/.cache/bun",
}];
const BUN_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Bun,
    aliases: &["bun"],
    roots: &BUN_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/bun",
    ],
    path_entries: &["/opt/mez/toolchains/bun/root/bin"],
    environment: &BUN_ENVIRONMENT,
    managed_state: &BUN_MANAGED_STATE,
    forbidden_descendants: &["install/global", "credentials", "config", ".npmrc"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const NODE_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "node-runtime",
    label: "Node.js distribution",
    sandbox_destination: SANDBOX_NODE_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/node"],
    required_directories: &["lib"],
}];
const NODE_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "NPM_CONFIG_CACHE",
        value: "/home/mez/.cache/npm",
    },
    ToolchainEnvironmentVariable {
        name: "COREPACK_HOME",
        value: "/home/mez/.cache/node/corepack",
    },
];
const NODE_MANAGED_STATE: [ManagedToolchainState; 2] = [
    ManagedToolchainState {
        purpose: "npm-cache",
        sandbox_path: "/home/mez/.cache/npm",
    },
    ManagedToolchainState {
        purpose: "corepack-cache",
        sandbox_path: "/home/mez/.cache/node/corepack",
    },
];
const NODE_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Node,
    aliases: &["node", "nodejs"],
    roots: &NODE_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/node",
    ],
    path_entries: &["/opt/mez/toolchains/node/root/bin"],
    environment: &NODE_ENVIRONMENT,
    managed_state: &NODE_MANAGED_STATE,
    forbidden_descendants: &[".npmrc", "credentials", "cache", "global"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const PYTHON_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "python-runtime",
    label: "Python runtime",
    sandbox_destination: SANDBOX_PYTHON_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/python3"],
    required_directories: &["lib"],
}];
const PYTHON_ENVIRONMENT: [ToolchainEnvironmentVariable; 3] = [
    ToolchainEnvironmentVariable {
        name: "PIP_CACHE_DIR",
        value: "/home/mez/.cache/pip",
    },
    ToolchainEnvironmentVariable {
        name: "UV_CACHE_DIR",
        value: "/home/mez/.cache/uv",
    },
    ToolchainEnvironmentVariable {
        name: "PYTHONNOUSERSITE",
        value: "1",
    },
];
const PYTHON_MANAGED_STATE: [ManagedToolchainState; 2] = [
    ManagedToolchainState {
        purpose: "pip-cache",
        sandbox_path: "/home/mez/.cache/pip",
    },
    ManagedToolchainState {
        purpose: "uv-cache",
        sandbox_path: "/home/mez/.cache/uv",
    },
];
const PYTHON_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Python,
    aliases: &["python", "python3"],
    roots: &PYTHON_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/python",
    ],
    path_entries: &["/opt/mez/toolchains/python/root/bin"],
    environment: &PYTHON_ENVIRONMENT,
    managed_state: &PYTHON_MANAGED_STATE,
    forbidden_descendants: &[
        "pip.conf",
        ".pypirc",
        "keyring",
        "credentials",
        "site-packages",
    ],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const JDK_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "jdk-runtime",
    label: "Java Development Kit",
    sandbox_destination: SANDBOX_JDK_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/java", "bin/javac", "bin/jar"],
    required_directories: &["lib"],
}];
const JDK_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "JAVA_HOME",
    value: SANDBOX_JDK_ROOT,
}];
const JDK_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Jdk,
    aliases: &["jdk", "java"],
    roots: &JDK_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/jdk",
    ],
    path_entries: &["/opt/mez/toolchains/jdk/root/bin"],
    environment: &JDK_ENVIRONMENT,
    managed_state: &[],
    forbidden_descendants: &[
        ".java",
        "credentials",
        "security/private",
        "maven",
        "gradle",
    ],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const MAVEN_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "maven-runtime",
    label: "Maven distribution",
    sandbox_destination: SANDBOX_MAVEN_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/mvn"],
    required_directories: &["lib", "boot"],
}];
const MAVEN_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "MAVEN_USER_HOME",
    value: "/home/mez/.m2",
}];
const MAVEN_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "maven-user-home",
    sandbox_path: "/home/mez/.m2",
}];
const MAVEN_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Maven,
    aliases: &["maven", "mvn"],
    roots: &MAVEN_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/maven",
    ],
    path_entries: &["/opt/mez/toolchains/maven/root/bin"],
    environment: &MAVEN_ENVIRONMENT,
    managed_state: &MAVEN_MANAGED_STATE,
    forbidden_descendants: &[
        ".m2",
        "settings.xml",
        "settings-security.xml",
        "credentials",
        ".sdkman",
        ".asdf",
        "mise",
    ],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Jdk],
        optional: &[],
    },
    allow_root_overlap: false,
};

const GRADLE_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "gradle-runtime",
    label: "Gradle distribution",
    sandbox_destination: SANDBOX_GRADLE_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/gradle"],
    required_directories: &["lib"],
}];
const GRADLE_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "GRADLE_USER_HOME",
        value: "/home/mez/.gradle",
    },
    ToolchainEnvironmentVariable {
        name: "GRADLE_OPTS",
        value: "-Dorg.gradle.daemon=false",
    },
];
const GRADLE_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "gradle-user-home",
    sandbox_path: "/home/mez/.gradle",
}];
const GRADLE_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Gradle,
    aliases: &["gradle"],
    roots: &GRADLE_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/gradle",
    ],
    path_entries: &["/opt/mez/toolchains/gradle/root/bin"],
    environment: &GRADLE_ENVIRONMENT,
    managed_state: &GRADLE_MANAGED_STATE,
    forbidden_descendants: &[
        ".gradle",
        "gradle.properties",
        "init.gradle",
        "init.d",
        "credentials",
        ".sdkman",
        ".asdf",
        "mise",
    ],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Jdk],
        optional: &[],
    },
    allow_root_overlap: false,
};

const DOTNET_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "dotnet-sdk",
    label: ".NET SDK",
    sandbox_destination: SANDBOX_DOTNET_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["dotnet"],
    required_directories: &["sdk", "shared", "packs"],
}];
const DOTNET_ENVIRONMENT: [ToolchainEnvironmentVariable; 6] = [
    ToolchainEnvironmentVariable {
        name: "DOTNET_ROOT",
        value: SANDBOX_DOTNET_ROOT,
    },
    ToolchainEnvironmentVariable {
        name: "DOTNET_CLI_HOME",
        value: "/home/mez/.dotnet",
    },
    ToolchainEnvironmentVariable {
        name: "NUGET_PACKAGES",
        value: "/home/mez/.cache/nuget/packages",
    },
    ToolchainEnvironmentVariable {
        name: "DOTNET_CLI_TELEMETRY_OPTOUT",
        value: "1",
    },
    ToolchainEnvironmentVariable {
        name: "DOTNET_SKIP_FIRST_TIME_EXPERIENCE",
        value: "1",
    },
    ToolchainEnvironmentVariable {
        name: "DOTNET_NOLOGO",
        value: "1",
    },
];
const DOTNET_MANAGED_STATE: [ManagedToolchainState; 2] = [
    ManagedToolchainState {
        purpose: "dotnet-cli-home",
        sandbox_path: "/home/mez/.dotnet",
    },
    ManagedToolchainState {
        purpose: "nuget-packages",
        sandbox_path: "/home/mez/.cache/nuget/packages",
    },
];
const DOTNET_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Dotnet,
    aliases: &["dotnet", ".net"],
    roots: &DOTNET_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/dotnet",
    ],
    path_entries: &[SANDBOX_DOTNET_ROOT],
    environment: &DOTNET_ENVIRONMENT,
    managed_state: &DOTNET_MANAGED_STATE,
    forbidden_descendants: &["NuGet.Config", "credentials", "tools", ".store"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const DART_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "dart-sdk",
    label: "Dart SDK",
    sandbox_destination: SANDBOX_DART_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/dart"],
    required_directories: &["lib"],
}];
const DART_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "PUB_CACHE",
    value: "/home/mez/.cache/dart-pub",
}];
const DART_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "dart-pub-cache",
    sandbox_path: "/home/mez/.cache/dart-pub",
}];
const DART_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Dart,
    aliases: &["dart"],
    roots: &DART_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/dart",
    ],
    path_entries: &["/opt/mez/toolchains/dart/root/bin"],
    environment: &DART_ENVIRONMENT,
    managed_state: &DART_MANAGED_STATE,
    forbidden_descendants: &["credentials.json", "global_packages", "flutter", "cache"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const KOTLIN_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "kotlin-jvm",
    label: "Kotlin/JVM compiler",
    sandbox_destination: SANDBOX_KOTLIN_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/kotlinc", "bin/kotlin"],
    required_directories: &["lib"],
}];
const KOTLIN_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Kotlin,
    aliases: &["kotlin", "kotlin-jvm"],
    roots: &KOTLIN_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/kotlin",
    ],
    path_entries: &["/opt/mez/toolchains/kotlin/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".gradle", "credentials", "android", "native", "js"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Jdk],
        optional: &[],
    },
    allow_root_overlap: false,
};

const RUBY_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "ruby-runtime",
    label: "Ruby runtime",
    sandbox_destination: SANDBOX_RUBY_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/ruby", "bin/gem", "bin/bundle"],
    required_directories: &["lib/ruby"],
}];
const RUBY_ENVIRONMENT: [ToolchainEnvironmentVariable; 6] = [
    ToolchainEnvironmentVariable {
        name: "GEM_HOME",
        value: "/home/mez/.local/share/ruby/gems",
    },
    ToolchainEnvironmentVariable {
        name: "GEM_PATH",
        value: "/home/mez/.local/share/ruby/gems",
    },
    ToolchainEnvironmentVariable {
        name: "BUNDLE_USER_HOME",
        value: "/home/mez/.local/share/bundle",
    },
    ToolchainEnvironmentVariable {
        name: "BUNDLE_USER_CACHE",
        value: "/home/mez/.cache/bundle",
    },
    ToolchainEnvironmentVariable {
        name: "BUNDLE_USER_CONFIG",
        value: "/home/mez/.config/bundle/config",
    },
    ToolchainEnvironmentVariable {
        name: "BUNDLE_USER_PLUGIN",
        value: "/home/mez/.local/share/bundle/plugin",
    },
];
const RUBY_MANAGED_STATE: [ManagedToolchainState; 4] = [
    ManagedToolchainState {
        purpose: "ruby-gems",
        sandbox_path: "/home/mez/.local/share/ruby/gems",
    },
    ManagedToolchainState {
        purpose: "bundler-home",
        sandbox_path: "/home/mez/.local/share/bundle",
    },
    ManagedToolchainState {
        purpose: "bundler-cache",
        sandbox_path: "/home/mez/.cache/bundle",
    },
    ManagedToolchainState {
        purpose: "bundler-config",
        sandbox_path: "/home/mez/.config/bundle",
    },
];
const RUBY_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Ruby,
    aliases: &["ruby"],
    roots: &RUBY_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/ruby",
    ],
    path_entries: &["/opt/mez/toolchains/ruby/root/bin"],
    environment: &RUBY_ENVIRONMENT,
    managed_state: &RUBY_MANAGED_STATE,
    forbidden_descendants: &["credentials", ".gem", ".bundle", "gemsets", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const PHP_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "php-runtime",
    label: "PHP runtime",
    sandbox_destination: SANDBOX_PHP_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/php"],
    required_directories: &["lib/php"],
}];
const PHP_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Php,
    aliases: &["php"],
    roots: &PHP_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/php",
    ],
    path_entries: &["/opt/mez/toolchains/php/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &["auth.json", "credentials", "global", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[SandboxToolchainKind::Composer],
    },
    allow_root_overlap: false,
};

const COMPOSER_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "composer-runtime",
    label: "Composer companion",
    sandbox_destination: SANDBOX_COMPOSER_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/composer"],
    required_directories: &[],
}];
const COMPOSER_ENVIRONMENT: [ToolchainEnvironmentVariable; 3] = [
    ToolchainEnvironmentVariable {
        name: "COMPOSER_HOME",
        value: "/home/mez/.config/composer",
    },
    ToolchainEnvironmentVariable {
        name: "COMPOSER_CACHE_DIR",
        value: "/home/mez/.cache/composer",
    },
    ToolchainEnvironmentVariable {
        name: "COMPOSER_VENDOR_DIR",
        value: "/home/mez/.local/share/composer/vendor",
    },
];
const COMPOSER_MANAGED_STATE: [ManagedToolchainState; 3] = [
    ManagedToolchainState {
        purpose: "composer-home",
        sandbox_path: "/home/mez/.config/composer",
    },
    ManagedToolchainState {
        purpose: "composer-cache",
        sandbox_path: "/home/mez/.cache/composer",
    },
    ManagedToolchainState {
        purpose: "composer-vendor",
        sandbox_path: "/home/mez/.local/share/composer/vendor",
    },
];
const COMPOSER_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Composer,
    aliases: &["composer"],
    roots: &COMPOSER_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/composer",
    ],
    path_entries: &["/opt/mez/toolchains/composer/root/bin"],
    environment: &COMPOSER_ENVIRONMENT,
    managed_state: &COMPOSER_MANAGED_STATE,
    forbidden_descendants: &["auth.json", "credentials", "keys", "certificates", "global"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Php],
        optional: &[],
    },
    allow_root_overlap: false,
};

const ERLANG_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "erlang-otp",
    label: "Erlang/OTP runtime",
    sandbox_destination: SANDBOX_ERLANG_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/erl", "bin/erlc", "bin/escript"],
    required_directories: &["lib/erlang"],
}];
const ERLANG_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Erlang,
    aliases: &["erlang"],
    roots: &ERLANG_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/erlang",
    ],
    path_entries: &["/opt/mez/toolchains/erlang/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".hex", ".mix", ".cache", "archives", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[SandboxToolchainKind::Elixir],
    },
    allow_root_overlap: false,
};

const ELIXIR_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "elixir-runtime",
    label: "Elixir runtime",
    sandbox_destination: SANDBOX_ELIXIR_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/elixir", "bin/elixirc", "bin/mix"],
    required_directories: &["lib/elixir"],
}];
const ELIXIR_ENVIRONMENT: [ToolchainEnvironmentVariable; 3] = [
    ToolchainEnvironmentVariable {
        name: "MIX_HOME",
        value: "/home/mez/.local/share/mix",
    },
    ToolchainEnvironmentVariable {
        name: "HEX_HOME",
        value: "/home/mez/.local/share/hex",
    },
    ToolchainEnvironmentVariable {
        name: "REBAR_CACHE_DIR",
        value: "/home/mez/.cache/rebar3",
    },
];
const ELIXIR_MANAGED_STATE: [ManagedToolchainState; 3] = [
    ManagedToolchainState {
        purpose: "mix-home",
        sandbox_path: "/home/mez/.local/share/mix",
    },
    ManagedToolchainState {
        purpose: "hex-home",
        sandbox_path: "/home/mez/.local/share/hex",
    },
    ManagedToolchainState {
        purpose: "rebar-cache",
        sandbox_path: "/home/mez/.cache/rebar3",
    },
];
const ELIXIR_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Elixir,
    aliases: &["elixir"],
    roots: &ELIXIR_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/elixir",
    ],
    path_entries: &["/opt/mez/toolchains/elixir/root/bin"],
    environment: &ELIXIR_ENVIRONMENT,
    managed_state: &ELIXIR_MANAGED_STATE,
    forbidden_descendants: &[".hex", ".mix", "archives", "credentials", "keys", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Erlang],
        optional: &[],
    },
    allow_root_overlap: false,
};

const GHC_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "ghc-compiler",
    label: "GHC compiler",
    sandbox_destination: SANDBOX_GHC_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/ghc", "bin/ghci", "bin/runghc", "bin/ghc-pkg"],
    required_directories: &["lib/ghc"],
}];
const GHC_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "GHC_ENVIRONMENT",
    value: "-",
}];
const GHC_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Ghc,
    aliases: &["ghc"],
    roots: &GHC_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/ghc",
    ],
    path_entries: &["/opt/mez/toolchains/ghc/root/bin"],
    environment: &GHC_ENVIRONMENT,
    managed_state: &[],
    forbidden_descendants: &[".ghcup", ".cabal", ".stack", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[SandboxToolchainKind::Cabal, SandboxToolchainKind::Stack],
    },
    allow_root_overlap: false,
};

const CABAL_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "cabal-companion",
    label: "Cabal companion",
    sandbox_destination: SANDBOX_CABAL_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/cabal"],
    required_directories: &[],
}];
const CABAL_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "CABAL_DIR",
    value: "/home/mez/.local/share/cabal",
}];
const CABAL_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "cabal-home",
    sandbox_path: "/home/mez/.local/share/cabal",
}];
const CABAL_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Cabal,
    aliases: &["cabal"],
    roots: &CABAL_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/cabal",
    ],
    path_entries: &["/opt/mez/toolchains/cabal/root/bin"],
    environment: &CABAL_ENVIRONMENT,
    managed_state: &CABAL_MANAGED_STATE,
    forbidden_descendants: &["config", "credentials", "packages", "store", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Ghc],
        optional: &[],
    },
    allow_root_overlap: false,
};

const STACK_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "stack-companion",
    label: "Stack companion",
    sandbox_destination: SANDBOX_STACK_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/stack"],
    required_directories: &[],
}];
const STACK_ENVIRONMENT: [ToolchainEnvironmentVariable; 1] = [ToolchainEnvironmentVariable {
    name: "STACK_ROOT",
    value: "/home/mez/.local/share/stack",
}];
const STACK_MANAGED_STATE: [ManagedToolchainState; 1] = [ManagedToolchainState {
    purpose: "stack-root",
    sandbox_path: "/home/mez/.local/share/stack",
}];
const STACK_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Stack,
    aliases: &["stack"],
    roots: &STACK_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/stack",
    ],
    path_entries: &["/opt/mez/toolchains/stack/root/bin"],
    environment: &STACK_ENVIRONMENT,
    managed_state: &STACK_MANAGED_STATE,
    forbidden_descendants: &[
        "config.yaml",
        "credentials",
        "programs",
        "snapshots",
        "shims",
    ],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[SandboxToolchainKind::Ghc],
        optional: &[],
    },
    allow_root_overlap: false,
};

const OCAML_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Ocaml,
    aliases: &["ocaml", "opam"],
    roots: &[],
    sandbox_directories: &[],
    path_entries: &[],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".opam", "credentials", "repositories", "plugins"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const LLVM_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "llvm-toolchain",
    label: "LLVM/Clang toolchain",
    sandbox_destination: SANDBOX_LLVM_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/clang", "bin/clang++", "bin/llvm-ar", "bin/llvm-config"],
    required_directories: &["lib/clang"],
}];
const LLVM_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Llvm,
    aliases: &["llvm", "clang"],
    roots: &LLVM_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/llvm",
    ],
    path_entries: &["/opt/mez/toolchains/llvm/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".linuxbrew", "Cellar", "Homebrew", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const GCC_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "gcc-toolchain",
    label: "GCC toolchain",
    sandbox_destination: SANDBOX_GCC_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &["bin/gcc", "bin/g++", "bin/gcc-ar"],
    required_directories: &["lib/gcc"],
}];
const GCC_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Gcc,
    aliases: &["gcc"],
    roots: &GCC_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/gcc",
    ],
    path_entries: &["/opt/mez/toolchains/gcc/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".linuxbrew", "Cellar", "Homebrew", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const CMAKE_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "cmake-toolchain",
    label: "CMake distribution",
    sandbox_destination: SANDBOX_CMAKE_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/cmake", "bin/ctest"],
    required_directories: &["share/cmake"],
}];
const CMAKE_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Cmake,
    aliases: &["cmake"],
    roots: &CMAKE_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/cmake",
    ],
    path_entries: &["/opt/mez/toolchains/cmake/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".linuxbrew", "Cellar", "Homebrew", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const NINJA_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "ninja-toolchain",
    label: "Ninja distribution",
    sandbox_destination: SANDBOX_NINJA_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/ninja"],
    required_directories: &[],
}];
const NINJA_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Ninja,
    aliases: &["ninja"],
    roots: &NINJA_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/ninja",
    ],
    path_entries: &["/opt/mez/toolchains/ninja/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".linuxbrew", "Cellar", "Homebrew", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const MESON_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "meson-toolchain",
    label: "Meson distribution",
    sandbox_destination: SANDBOX_MESON_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::UserTools,
    required_executables: &["bin/meson"],
    required_directories: &[],
}];
const MESON_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Meson,
    aliases: &["meson"],
    roots: &MESON_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/meson",
    ],
    path_entries: &["/opt/mez/toolchains/meson/root/bin"],
    environment: &[],
    managed_state: &[],
    forbidden_descendants: &[".linuxbrew", "Cellar", "Homebrew", "credentials", "shims"],
    platform: ToolchainPlatform::Any,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[],
    },
    allow_root_overlap: false,
};

const SWIFT_ROOTS: [ToolchainRootDescriptor; 1] = [ToolchainRootDescriptor {
    evidence_kind: "swift-toolchain",
    label: "Swift Linux toolchain",
    sandbox_destination: SANDBOX_SWIFT_ROOT,
    allowed_names: &[],
    allowed_parent_names: &[],
    authority_class: ToolchainAuthorityClass::Runtime,
    required_executables: &[
        "bin/swift",
        "bin/swiftc",
        "bin/swift-package",
        "bin/sourcekit-lsp",
    ],
    required_directories: &["lib/swift/linux"],
}];
const SWIFT_ENVIRONMENT: [ToolchainEnvironmentVariable; 2] = [
    ToolchainEnvironmentVariable {
        name: "SWIFTPM_CACHE_PATH",
        value: "/home/mez/.cache/swiftpm",
    },
    ToolchainEnvironmentVariable {
        name: "SWIFTPM_CONFIG_PATH",
        value: "/home/mez/.config/swiftpm",
    },
];
const SWIFT_MANAGED_STATE: [ManagedToolchainState; 3] = [
    ManagedToolchainState {
        purpose: "swiftpm-cache",
        sandbox_path: "/home/mez/.cache/swiftpm",
    },
    ManagedToolchainState {
        purpose: "swiftpm-config",
        sandbox_path: "/home/mez/.config/swiftpm",
    },
    ManagedToolchainState {
        purpose: "swift-build-state",
        sandbox_path: "/home/mez/.local/state/swiftpm",
    },
];
const SWIFT_DESCRIPTOR: ToolchainDescriptor = ToolchainDescriptor {
    kind: SandboxToolchainKind::Swift,
    aliases: &["swift"],
    roots: &SWIFT_ROOTS,
    sandbox_directories: &[
        "/opt",
        "/opt/mez",
        "/opt/mez/toolchains",
        "/opt/mez/toolchains/swift",
    ],
    path_entries: &["/opt/mez/toolchains/swift/root/bin"],
    environment: &SWIFT_ENVIRONMENT,
    managed_state: &SWIFT_MANAGED_STATE,
    forbidden_descendants: &[
        ".swiftenv",
        ".asdf",
        ".local/share/mise",
        "Xcode.app",
        "Developer",
        "credentials",
        "shims",
    ],
    platform: ToolchainPlatform::Linux,
    coupling: ToolchainCoupling {
        required: &[],
        optional: &[
            SandboxToolchainKind::Llvm,
            SandboxToolchainKind::Cmake,
            SandboxToolchainKind::Ninja,
        ],
    },
    allow_root_overlap: false,
};

/// One validated host root and its fixed sandbox destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchainRoot {
    /// Security class inherited from the descriptor.
    pub(crate) authority_class: ToolchainAuthorityClass,
    /// Canonical host source from pane bootstrap evidence.
    pub(crate) host_path: PathBuf,
    /// Fixed code-owned sandbox destination.
    pub(crate) sandbox_destination: String,
}

/// One descriptor resolved from active-pane evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchain {
    /// Typed descriptor kind.
    pub(crate) kind: SandboxToolchainKind,
    /// Validated roots in descriptor order.
    pub(crate) roots: Vec<ResolvedToolchainRoot>,
}

/// One validated repository-contained executable environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedProjectEnvironment {
    /// Toolchain kind that owns the environment contract.
    pub(crate) kind: SandboxToolchainKind,
    /// Canonical host path already projected through trusted project authority.
    pub(crate) host_path: PathBuf,
    /// Sandbox-visible path, identical to the trusted project path.
    pub(crate) sandbox_path: String,
    /// Optional executable directory prepended for directory-style environments.
    pub(crate) path_entry: Option<String>,
}

/// Deterministically composed projection consumed by Bubblewrap compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolchainProjection {
    /// Built-in kinds in configured selection order.
    pub(crate) kinds: Vec<SandboxToolchainKind>,
    /// Custom identities in configured selection order.
    pub(crate) custom_names: Vec<String>,
    /// Parent directories created before mounts.
    pub(crate) sandbox_directories: Vec<String>,
    /// Validated fixed read-only mounts.
    pub(crate) roots: Vec<ResolvedToolchainRoot>,
    /// Ordered executable search paths excluding the system suffix.
    pub(crate) path_entries: Vec<String>,
    /// Explicit synthesized environment excluding PATH.
    pub(crate) environment: BTreeMap<String, String>,
    /// Managed-state declarations for status and future quotas.
    pub(crate) managed_state: Vec<ManagedToolchainState>,
    /// Repository-contained executable environments that reuse project authority.
    pub(crate) project_environments: Vec<ResolvedProjectEnvironment>,
    /// Stable digest sealing all resolved projection metadata after filesystem validation.
    integrity_sha256: String,
}

impl ResolvedToolchainProjection {
    /// Seals the fully resolved projection after filesystem-backed validation.
    fn seal(&mut self) {
        self.integrity_sha256 = self.metadata_sha256();
    }

    /// Computes a stable digest over every launch-relevant projection field.
    fn metadata_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"mez-resolved-toolchain-projection-v1\0");
        for kind in &self.kinds {
            digest.update(kind.as_str().as_bytes());
            digest.update([0]);
        }
        for name in &self.custom_names {
            digest.update(b"custom:");
            digest.update(name.as_bytes());
            digest.update([0]);
        }
        for directory in &self.sandbox_directories {
            digest.update(directory.as_bytes());
            digest.update([0]);
        }
        for root in &self.roots {
            digest.update(match root.authority_class {
                ToolchainAuthorityClass::Runtime => b"runtime".as_slice(),
                ToolchainAuthorityClass::ProjectEnvironment => b"project-environment".as_slice(),
                ToolchainAuthorityClass::UserTools => b"user-tools".as_slice(),
                ToolchainAuthorityClass::ManagedState => b"managed-state".as_slice(),
                ToolchainAuthorityClass::Credential => b"credential".as_slice(),
            });
            digest.update([0]);
            digest.update(root.host_path.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(root.sandbox_destination.as_bytes());
            digest.update([0]);
        }
        for entry in &self.path_entries {
            digest.update(entry.as_bytes());
            digest.update([0]);
        }
        for (name, value) in &self.environment {
            digest.update(name.as_bytes());
            digest.update([0]);
            digest.update(value.as_bytes());
            digest.update([0]);
        }
        for state in &self.managed_state {
            digest.update(state.purpose.as_bytes());
            digest.update([0]);
            digest.update(state.sandbox_path.as_bytes());
            digest.update([0]);
        }
        for environment in &self.project_environments {
            digest.update(environment.kind.as_str().as_bytes());
            digest.update([0]);
            digest.update(environment.host_path.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(environment.sandbox_path.as_bytes());
            digest.update([0]);
            if let Some(path_entry) = &environment.path_entry {
                digest.update(path_entry.as_bytes());
            }
            digest.update([0]);
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("")
    }

    /// Validates every projected host root against pane-resolved maximum read authority.
    pub(crate) fn validate_authority(
        &self,
        authority: &PathScopes,
    ) -> Result<(), SandboxCompileError> {
        for root in &self.roots {
            if !authority
                .read_scopes
                .iter()
                .any(|scope| root.host_path.starts_with(Path::new(scope)))
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ToolchainOutsideAuthority,
                    format!(
                        "{} falls outside maximum sandbox read authority",
                        root.sandbox_destination
                    ),
                ));
            }
        }
        for environment in &self.project_environments {
            if !authority
                .read_scopes
                .iter()
                .any(|scope| environment.host_path.starts_with(Path::new(scope)))
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ToolchainOutsideAuthority,
                    "project toolchain environment falls outside maximum sandbox read authority",
                ));
            }
        }
        Ok(())
    }

    /// Adds enabled projection roots to effective read authority without
    /// changing generic filesystem mounts or write authority.
    pub(crate) fn extend_read_authority(
        &self,
        authority: &PathScopes,
    ) -> Result<PathScopes, SandboxCompileError> {
        let read_scopes = authority
            .read_scopes
            .iter()
            .cloned()
            .chain(
                self.roots
                    .iter()
                    .map(|root| root.host_path.to_string_lossy().into_owned()),
            )
            .chain(
                self.project_environments
                    .iter()
                    .map(|environment| environment.host_path.to_string_lossy().into_owned()),
            )
            .collect();
        PathScopes::try_shell_resolved_with_evidence(
            authority.current_directory.clone(),
            read_scopes,
            authority.write_scopes.clone(),
            authority.path_evidence.clone(),
        )
        .map_err(|error| {
            SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, error.message())
        })
    }

    /// Revalidates descriptor-owned roots before final launch compilation.
    pub(crate) fn validate(&self) -> Result<(), SandboxCompileError> {
        let kinds = self.kinds.iter().copied().collect::<BTreeSet<_>>();
        if kinds.len() != self.kinds.len() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection contains duplicate kinds",
            ));
        }
        let custom_names = self.custom_names.iter().collect::<BTreeSet<_>>();
        if custom_names.len() != self.custom_names.len() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection contains duplicate custom identities",
            ));
        }
        let mut expected_root_count = 0;
        let mut expected_directories = Vec::new();
        let mut expected_path_entries = Vec::new();
        let mut expected_environment = BTreeMap::new();
        let mut expected_managed_state = Vec::new();
        for kind in &self.kinds {
            let descriptor = toolchain_descriptor(*kind);
            let wrapper_backed = self.project_environments.iter().any(|environment| {
                environment.kind == *kind
                    && matches!(
                        kind,
                        SandboxToolchainKind::Maven | SandboxToolchainKind::Gradle
                    )
            });
            if !wrapper_backed {
                expected_root_count += descriptor.roots.len();
            }
            for directory in descriptor.sandbox_directories {
                if !expected_directories
                    .iter()
                    .any(|expected| expected == directory)
                {
                    expected_directories.push((*directory).to_string());
                }
            }
            if !wrapper_backed {
                expected_path_entries.extend(
                    descriptor
                        .path_entries
                        .iter()
                        .map(|entry| (*entry).to_string()),
                );
            }
            for variable in descriptor.environment {
                if expected_environment
                    .insert(variable.name.to_string(), variable.value.to_string())
                    .is_some()
                {
                    return Err(SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        format!(
                            "resolved toolchain projection contains duplicate {} metadata",
                            variable.name
                        ),
                    ));
                }
            }
            expected_managed_state.extend_from_slice(descriptor.managed_state);
            for expected in descriptor.roots.iter().filter(|_| !wrapper_backed) {
                let root = self
                    .roots
                    .iter()
                    .find(|root| root.sandbox_destination == expected.sandbox_destination)
                    .ok_or_else(|| {
                        SandboxCompileError::new(
                            SandboxCompileErrorKind::InvalidInput,
                            format!(
                                "resolved {} projection is missing {}",
                                kind.as_str(),
                                expected.label
                            ),
                        )
                    })?;
                validate_descriptor_root(&root.host_path, expected)?;
                if root.authority_class != expected.authority_class
                    || matches!(
                        root.authority_class,
                        ToolchainAuthorityClass::ManagedState | ToolchainAuthorityClass::Credential
                    )
                {
                    return Err(SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        format!(
                            "resolved {} projection has an invalid authority class",
                            expected.label
                        ),
                    ));
                }
            }
        }
        for environment in &self.project_environments {
            if !self.kinds.contains(&environment.kind) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "resolved project environment has no matching selected toolchain",
                ));
            }
            match environment.kind {
                SandboxToolchainKind::Python => {
                    validate_python_project_environment(&environment.host_path)?;
                }
                SandboxToolchainKind::Ocaml => {
                    validate_ocaml_project_environment(&environment.host_path)?;
                }
                SandboxToolchainKind::Maven => {
                    validate_jvm_project_wrapper(
                        &environment.host_path,
                        SandboxToolchainKind::Maven,
                    )?;
                }
                SandboxToolchainKind::Gradle => {
                    validate_jvm_project_wrapper(
                        &environment.host_path,
                        SandboxToolchainKind::Gradle,
                    )?;
                }
                _ => {
                    return Err(SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        "resolved project environment has an unsupported toolchain kind",
                    ));
                }
            }
            if environment.sandbox_path != environment.host_path.display().to_string() {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "resolved project environment has an invalid sandbox path",
                ));
            }
            let expected_path_entry = match environment.kind {
                SandboxToolchainKind::Python | SandboxToolchainKind::Ocaml => {
                    Some(format!("{}/bin", environment.sandbox_path))
                }
                SandboxToolchainKind::Maven | SandboxToolchainKind::Gradle => None,
                _ => unreachable!("project environment kinds were validated above"),
            };
            if environment.path_entry != expected_path_entry {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "resolved project environment has an invalid executable path",
                ));
            }
        }
        if self.custom_names.is_empty()
            && (self.roots.len() != expected_root_count
                || self.sandbox_directories != expected_directories
                || self.path_entries != expected_path_entries
                || self.environment != expected_environment
                || self.managed_state != expected_managed_state)
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection does not match descriptor metadata",
            ));
        }
        for (index, root) in self.roots.iter().enumerate() {
            if self.roots.iter().skip(index + 1).any(|other| {
                root.sandbox_destination == other.sandbox_destination
                    || root.host_path == other.host_path
                    || root.host_path.starts_with(&other.host_path)
                    || other.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "resolved toolchain projection contains colliding mounts",
                ));
            }
        }
        if self.integrity_sha256 != self.metadata_sha256() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "resolved toolchain projection failed its integrity check",
            ));
        }
        Ok(())
    }

    /// Builds deterministic PATH with the fixed system suffix.
    pub(crate) fn executable_path(&self) -> String {
        self.project_environments
            .iter()
            .filter_map(|environment| environment.path_entry.clone())
            .chain(self.path_entries.iter().map(|entry| (*entry).to_string()))
            .chain(["/usr/bin".to_string(), "/bin".to_string()])
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Returns stable descriptor metadata for one allowlisted kind.
pub(crate) const fn toolchain_descriptor(
    kind: SandboxToolchainKind,
) -> &'static ToolchainDescriptor {
    match kind {
        SandboxToolchainKind::Rust => &RUST_DESCRIPTOR,
        SandboxToolchainKind::Zig => &ZIG_DESCRIPTOR,
        SandboxToolchainKind::Go => &GO_DESCRIPTOR,
        SandboxToolchainKind::Deno => &DENO_DESCRIPTOR,
        SandboxToolchainKind::Bun => &BUN_DESCRIPTOR,
        SandboxToolchainKind::Node => &NODE_DESCRIPTOR,
        SandboxToolchainKind::Python => &PYTHON_DESCRIPTOR,
        SandboxToolchainKind::Jdk => &JDK_DESCRIPTOR,
        SandboxToolchainKind::Maven => &MAVEN_DESCRIPTOR,
        SandboxToolchainKind::Gradle => &GRADLE_DESCRIPTOR,
        SandboxToolchainKind::Dotnet => &DOTNET_DESCRIPTOR,
        SandboxToolchainKind::Dart => &DART_DESCRIPTOR,
        SandboxToolchainKind::Kotlin => &KOTLIN_DESCRIPTOR,
        SandboxToolchainKind::Ruby => &RUBY_DESCRIPTOR,
        SandboxToolchainKind::Php => &PHP_DESCRIPTOR,
        SandboxToolchainKind::Composer => &COMPOSER_DESCRIPTOR,
        SandboxToolchainKind::Erlang => &ERLANG_DESCRIPTOR,
        SandboxToolchainKind::Elixir => &ELIXIR_DESCRIPTOR,
        SandboxToolchainKind::Ghc => &GHC_DESCRIPTOR,
        SandboxToolchainKind::Cabal => &CABAL_DESCRIPTOR,
        SandboxToolchainKind::Stack => &STACK_DESCRIPTOR,
        SandboxToolchainKind::Ocaml => &OCAML_DESCRIPTOR,
        SandboxToolchainKind::Llvm => &LLVM_DESCRIPTOR,
        SandboxToolchainKind::Gcc => &GCC_DESCRIPTOR,
        SandboxToolchainKind::Cmake => &CMAKE_DESCRIPTOR,
        SandboxToolchainKind::Ninja => &NINJA_DESCRIPTOR,
        SandboxToolchainKind::Meson => &MESON_DESCRIPTOR,
        SandboxToolchainKind::Swift => &SWIFT_DESCRIPTOR,
    }
}

/// Resolves and composes every selected descriptor from active-pane evidence.
pub(crate) fn resolve_toolchain_projection(
    selected: &[SandboxToolchainKind],
    environment_managers: &[String],
    host_os: &str,
) -> Result<Option<ResolvedToolchainProjection>, SandboxCompileError> {
    resolve_toolchain_projection_for_project(selected, environment_managers, host_os, None)
}

/// Resolves selected descriptors and an optional trusted-project environment.
pub(crate) fn resolve_toolchain_projection_for_project(
    selected: &[SandboxToolchainKind],
    environment_managers: &[String],
    host_os: &str,
    trusted_project_root: Option<&Path>,
) -> Result<Option<ResolvedToolchainProjection>, SandboxCompileError> {
    if selected.is_empty() {
        return Ok(None);
    }
    let selected_set = selected.iter().copied().collect::<BTreeSet<_>>();
    if selected_set.len() != selected.len() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "selected toolchain kinds must not contain duplicates",
        ));
    }
    let mut resolved = Vec::new();
    let mut project_environments = Vec::new();
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS {
        if !selected_set.contains(&kind) {
            continue;
        }
        let descriptor = toolchain_descriptor(kind);
        if !descriptor.platform.supports(host_os) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::UnsupportedRequirement,
                format!("{} toolchain is unsupported on {host_os}", kind.as_str()),
            ));
        }
        for required in descriptor.coupling.required {
            if !selected_set.contains(required) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::UnsupportedRequirement,
                    format!(
                        "{} toolchain requires selected companion {}",
                        kind.as_str(),
                        required.as_str()
                    ),
                ));
            }
        }
        let (toolchain, project_environment) = resolve_descriptor_with_project_wrapper(
            descriptor,
            environment_managers,
            trusted_project_root,
        )?;
        resolved.push(toolchain);
        if let Some(project_environment) = project_environment {
            project_environments.push(project_environment);
        }
    }
    let mut projection = compose_toolchain_projection(&resolved)?;
    projection.project_environments.extend(project_environments);
    if selected_set.contains(&SandboxToolchainKind::Python)
        && let Some(project_root) = trusted_project_root
        && let Some(environment) = resolve_python_project_environment(project_root)?
    {
        projection.project_environments.push(environment);
    }
    append_required_ocaml_project_environment(
        &mut projection,
        &selected_set,
        trusted_project_root,
    )?;
    projection.seal();
    projection.validate()?;
    Ok(Some(projection))
}

/// Resolves ordered built-in and primary-user custom selections into one
/// read-only projection without creating or modifying host state.
pub(crate) fn resolve_configured_toolchain_projection_for_project(
    config: &BubblewrapConfig,
    environment_managers: &[String],
    host_os: &str,
    trusted_project_root: Option<&Path>,
    protected_host_roots: &[PathBuf],
) -> Result<Option<ResolvedToolchainProjection>, SandboxCompileError> {
    if config.toolchain_selections.is_empty() {
        return Ok(None);
    }
    let selected_built_ins = config
        .toolchain_selections
        .iter()
        .filter_map(|selection| match selection {
            ToolchainSelection::BuiltIn(kind) => Some(*kind),
            ToolchainSelection::Custom(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let mut projection = empty_toolchain_projection();
    for selection in &config.toolchain_selections {
        match selection {
            ToolchainSelection::BuiltIn(kind) => {
                let descriptor = toolchain_descriptor(*kind);
                validate_selected_descriptor(descriptor, &selected_built_ins, host_os)?;
                let (resolved, project_environment) = resolve_descriptor_with_project_wrapper(
                    descriptor,
                    environment_managers,
                    trusted_project_root,
                )?;
                append_resolved_descriptor(&mut projection, &resolved)?;
                if let Some(project_environment) = project_environment {
                    projection.project_environments.push(project_environment);
                }
            }
            ToolchainSelection::Custom(name) => {
                let definition = config.custom_toolchains.get(name.name()).ok_or_else(|| {
                    SandboxCompileError::new(
                        SandboxCompileErrorKind::InvalidInput,
                        format!("missing custom toolchain definition for `{}`", name.name()),
                    )
                })?;
                append_custom_toolchain(
                    &mut projection,
                    name.name(),
                    definition,
                    protected_host_roots,
                )?;
            }
        }
    }
    if selected_built_ins.contains(&SandboxToolchainKind::Python)
        && let Some(project_root) = trusted_project_root
        && let Some(environment) = resolve_python_project_environment(project_root)?
    {
        projection.project_environments.push(environment);
    }
    append_required_ocaml_project_environment(
        &mut projection,
        &selected_built_ins,
        trusted_project_root,
    )?;
    projection.seal();
    projection.validate()?;
    Ok(Some(projection))
}

/// Validates platform and companion requirements for one selected descriptor.
fn validate_selected_descriptor(
    descriptor: &ToolchainDescriptor,
    selected: &BTreeSet<SandboxToolchainKind>,
    host_os: &str,
) -> Result<(), SandboxCompileError> {
    if !descriptor.platform.supports(host_os) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            format!(
                "{} toolchain is unsupported on {host_os}",
                descriptor.kind.as_str()
            ),
        ));
    }
    for required in descriptor.coupling.required {
        if !selected.contains(required) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::UnsupportedRequirement,
                format!(
                    "{} toolchain requires selected companion {}",
                    descriptor.kind.as_str(),
                    required.as_str()
                ),
            ));
        }
    }
    Ok(())
}

/// Constructs an unsealed projection for ordered descriptor composition.
fn empty_toolchain_projection() -> ResolvedToolchainProjection {
    ResolvedToolchainProjection {
        kinds: Vec::new(),
        custom_names: Vec::new(),
        sandbox_directories: Vec::new(),
        roots: Vec::new(),
        path_entries: Vec::new(),
        environment: BTreeMap::new(),
        managed_state: Vec::new(),
        project_environments: Vec::new(),
        integrity_sha256: String::new(),
    }
}

/// Appends one validated built-in descriptor in configured selection order.
fn append_resolved_descriptor(
    projection: &mut ResolvedToolchainProjection,
    resolved: &ResolvedToolchain,
) -> Result<(), SandboxCompileError> {
    if projection.kinds.contains(&resolved.kind) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "resolved toolchain projection contains duplicate kinds",
        ));
    }
    let descriptor = toolchain_descriptor(resolved.kind);
    projection.kinds.push(resolved.kind);
    for directory in descriptor.sandbox_directories {
        push_unique(&mut projection.sandbox_directories, directory);
    }
    for root in &resolved.roots {
        append_projection_root(projection, root.clone())?;
    }
    if !resolved.roots.is_empty() {
        for entry in descriptor.path_entries {
            push_unique(&mut projection.path_entries, entry);
        }
    }
    for variable in descriptor.environment {
        insert_projection_environment(projection, variable.name, variable.value)?;
    }
    for state in descriptor.managed_state {
        if projection
            .managed_state
            .iter()
            .any(|existing| existing.sandbox_path == state.sandbox_path)
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!(
                    "toolchain descriptors collide in managed state at {}",
                    state.sandbox_path
                ),
            ));
        }
        projection.managed_state.push(*state);
    }
    Ok(())
}

/// Resolves and appends one constrained custom definition.
fn append_custom_toolchain(
    projection: &mut ResolvedToolchainProjection,
    name: &str,
    definition: &CustomToolchainDefinition,
    protected_host_roots: &[PathBuf],
) -> Result<(), SandboxCompileError> {
    if projection
        .custom_names
        .iter()
        .any(|existing| existing == name)
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("duplicate custom toolchain selection `{name}`"),
        ));
    }
    let base = format!("/opt/mez/toolchains/custom/{name}");
    for directory in [
        "/opt".to_string(),
        "/opt/mez".to_string(),
        "/opt/mez/toolchains".to_string(),
        "/opt/mez/toolchains/custom".to_string(),
        base.clone(),
        format!("{base}/roots"),
    ] {
        push_unique(&mut projection.sandbox_directories, &directory);
    }

    let mut roots = Vec::with_capacity(definition.roots.len());
    for (index, configured) in definition.roots.iter().enumerate() {
        let root = resolve_custom_root(configured, name, protected_host_roots)?;
        let destination = format!("{base}/roots/{index}");
        append_projection_root(
            projection,
            ResolvedToolchainRoot {
                authority_class: ToolchainAuthorityClass::UserTools,
                host_path: root.clone(),
                sandbox_destination: destination,
            },
        )?;
        roots.push(root);
    }
    for reference in &definition.path_entries {
        validate_custom_reference_target(&roots, reference, name, CustomReferenceKind::Directory)?;
        let value = custom_reference_sandbox_path(&base, reference);
        push_unique(&mut projection.path_entries, &value);
    }
    for reference in &definition.required_executables {
        validate_custom_reference_target(&roots, reference, name, CustomReferenceKind::Executable)?;
    }
    for (variable, reference) in &definition.environment {
        validate_custom_reference_target(&roots, reference, name, CustomReferenceKind::Existing)?;
        insert_projection_environment(
            projection,
            variable,
            &custom_reference_sandbox_path(&base, reference),
        )?;
    }
    projection.custom_names.push(name.to_string());
    Ok(())
}

/// Filesystem shape required by one resolved custom reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomReferenceKind {
    Existing,
    Directory,
    Executable,
}

/// Resolves one custom root without following a root-level symlink.
fn resolve_custom_root(
    configured: &str,
    name: &str,
    protected_host_roots: &[PathBuf],
) -> Result<PathBuf, SandboxCompileError> {
    let path = Path::new(configured);
    validate_toolchain_root(path, &format!("custom toolchain `{name}` root"), &[])?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect custom toolchain `{name}` root: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("custom toolchain `{name}` root must be a real directory"),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize custom toolchain `{name}` root: {error}"),
        )
    })?;
    if canonical != path {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("custom toolchain `{name}` root must use its canonical path"),
        ));
    }
    let rendered = canonical.to_string_lossy();
    let complete_home = canonical
        .parent()
        .is_some_and(|parent| parent == Path::new("/home"));
    if complete_home
        || [
            "/root", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt", "/var", "/proc",
            "/dev", "/run", "/tmp",
        ]
        .iter()
        .any(|forbidden| rendered == *forbidden)
        || protected_host_roots
            .iter()
            .any(|protected| canonical.starts_with(protected) || protected.starts_with(&canonical))
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("custom toolchain `{name}` root overlaps a forbidden host path"),
        ));
    }
    Ok(canonical)
}

/// Resolves and validates one custom root-relative reference.
fn validate_custom_reference_target(
    roots: &[PathBuf],
    reference: &CustomToolchainReference,
    name: &str,
    kind: CustomReferenceKind,
) -> Result<(), SandboxCompileError> {
    let root = roots.get(reference.root_index).ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("custom toolchain `{name}` reference has an invalid root index"),
        )
    })?;
    let target = if reference.relative_path == "." {
        root.clone()
    } else {
        root.join(&reference.relative_path)
    };
    let canonical = target.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to resolve custom toolchain `{name}` reference: {error}"),
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("custom toolchain `{name}` reference escapes its declared root"),
        ));
    }
    let metadata = fs::symlink_metadata(&target).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect custom toolchain `{name}` reference: {error}"),
        )
    })?;
    let valid = match kind {
        CustomReferenceKind::Existing => {
            !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir())
        }
        CustomReferenceKind::Directory => metadata.is_dir(),
        CustomReferenceKind::Executable => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                !metadata.file_type().is_symlink() && metadata.is_file()
            }
        }
    };
    if !valid {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("custom toolchain `{name}` reference has an invalid filesystem type"),
        ));
    }
    Ok(())
}

/// Renders one validated custom reference as a fixed sandbox path.
fn custom_reference_sandbox_path(base: &str, reference: &CustomToolchainReference) -> String {
    let root = format!("{base}/roots/{}", reference.root_index);
    if reference.relative_path == "." {
        root
    } else {
        format!("{root}/{}", reference.relative_path)
    }
}

/// Appends one root while rejecting host overlap and destination collision.
fn append_projection_root(
    projection: &mut ResolvedToolchainProjection,
    root: ResolvedToolchainRoot,
) -> Result<(), SandboxCompileError> {
    if projection.roots.iter().any(|existing| {
        existing.sandbox_destination == root.sandbox_destination
            || existing.host_path == root.host_path
            || existing.host_path.starts_with(&root.host_path)
            || root.host_path.starts_with(&existing.host_path)
    }) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "toolchain projections contain overlapping roots or destinations",
        ));
    }
    projection.roots.push(root);
    Ok(())
}

/// Inserts one synthesized value, accepting only byte-identical duplicates.
fn insert_projection_environment(
    projection: &mut ResolvedToolchainProjection,
    name: &str,
    value: &str,
) -> Result<(), SandboxCompileError> {
    if let Some(existing) = projection.environment.get(name) {
        if existing == value {
            return Ok(());
        }
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("toolchain projections synthesize conflicting {name} values"),
        ));
    }
    projection
        .environment
        .insert(name.to_string(), value.to_string());
    Ok(())
}

/// Appends one value only when an identical value is not already present.
fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

/// Resolves one descriptor from bounded pane-bootstrap evidence.
fn resolve_descriptor(
    descriptor: &ToolchainDescriptor,
    environment_managers: &[String],
) -> Result<ResolvedToolchain, SandboxCompileError> {
    let mut roots = Vec::with_capacity(descriptor.roots.len());
    for root_descriptor in descriptor.roots {
        let host_path = unique_manager_path(environment_managers, root_descriptor.evidence_kind)?
            .ok_or_else(|| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::UnsupportedRequirement,
                format!(
                    "selected {} toolchain requires {} from pane bootstrap",
                    descriptor.kind.as_str(),
                    root_descriptor.label
                ),
            )
        })?;
        validate_descriptor_root(&host_path, root_descriptor)?;
        if descriptor.forbidden_descendants.iter().any(|forbidden| {
            host_path.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|component| component == *forbidden)
            })
        }) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!(
                    "{} toolchain root contains a forbidden credential or configuration component",
                    descriptor.kind.as_str()
                ),
            ));
        }
        roots.push(ResolvedToolchainRoot {
            authority_class: root_descriptor.authority_class,
            host_path,
            sandbox_destination: root_descriptor.sandbox_destination.to_string(),
        });
    }
    if !descriptor.allow_root_overlap {
        for (index, root) in roots.iter().enumerate() {
            if roots.iter().skip(index + 1).any(|other| {
                root.host_path == other.host_path
                    || root.host_path.starts_with(&other.host_path)
                    || other.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ForbiddenHostPath,
                    format!(
                        "{} toolchain roots must be distinct and non-overlapping",
                        descriptor.kind.as_str()
                    ),
                ));
            }
        }
    }
    Ok(ResolvedToolchain {
        kind: descriptor.kind,
        roots,
    })
}

/// Prefers a validated repository wrapper for JVM build tools and otherwise
/// resolves the selected standalone distribution from pane bootstrap evidence.
fn resolve_descriptor_with_project_wrapper(
    descriptor: &ToolchainDescriptor,
    environment_managers: &[String],
    trusted_project_root: Option<&Path>,
) -> Result<(ResolvedToolchain, Option<ResolvedProjectEnvironment>), SandboxCompileError> {
    if matches!(
        descriptor.kind,
        SandboxToolchainKind::Maven | SandboxToolchainKind::Gradle
    ) && let Some(project_root) = trusted_project_root
        && let Some(environment) = discover_jvm_project_wrapper(project_root, descriptor.kind)?
    {
        return Ok((
            ResolvedToolchain {
                kind: descriptor.kind,
                roots: Vec::new(),
            },
            Some(environment),
        ));
    }
    resolve_descriptor(descriptor, environment_managers).map(|resolved| (resolved, None))
}

/// Composes resolved descriptors in stable descriptor priority order.
fn compose_toolchain_projection(
    resolved: &[ResolvedToolchain],
) -> Result<ResolvedToolchainProjection, SandboxCompileError> {
    let resolved_by_kind = resolved
        .iter()
        .map(|toolchain| (toolchain.kind, toolchain))
        .collect::<BTreeMap<_, _>>();
    if resolved_by_kind.len() != resolved.len() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "resolved toolchain kinds must not contain duplicates",
        ));
    }

    let mut projection = ResolvedToolchainProjection {
        kinds: Vec::new(),
        custom_names: Vec::new(),
        sandbox_directories: Vec::new(),
        roots: Vec::new(),
        path_entries: Vec::new(),
        environment: BTreeMap::new(),
        managed_state: Vec::new(),
        project_environments: Vec::new(),
        integrity_sha256: String::new(),
    };
    let mut destinations = BTreeSet::<String>::new();
    let mut managed_paths = BTreeSet::new();
    for kind in SUPPORTED_SANDBOX_TOOLCHAIN_KINDS {
        let Some(toolchain) = resolved_by_kind.get(&kind) else {
            continue;
        };
        let descriptor = toolchain_descriptor(kind);
        for optional in descriptor.coupling.optional {
            if resolved_by_kind.contains_key(optional) {
                let _ = toolchain_descriptor(*optional);
            }
        }
        projection.kinds.push(kind);
        for directory in descriptor.sandbox_directories {
            if !projection
                .sandbox_directories
                .iter()
                .any(|existing| existing == directory)
            {
                projection
                    .sandbox_directories
                    .push((*directory).to_string());
            }
        }
        for root in &toolchain.roots {
            if !destinations.insert(root.sandbox_destination.clone()) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors collide at fixed destination {}",
                        root.sandbox_destination
                    ),
                ));
            }
            if projection.roots.iter().any(|existing| {
                root.host_path == existing.host_path
                    || root.host_path.starts_with(&existing.host_path)
                    || existing.host_path.starts_with(&root.host_path)
            }) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::ForbiddenHostPath,
                    "toolchain descriptors resolve overlapping host roots",
                ));
            }
            projection.roots.push(root.clone());
        }
        for path_entry in descriptor
            .path_entries
            .iter()
            .filter(|_| !toolchain.roots.is_empty())
        {
            if projection
                .path_entries
                .iter()
                .any(|existing| existing == path_entry)
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("toolchain descriptors collide in PATH at {path_entry}"),
                ));
            }
            projection.path_entries.push((*path_entry).to_string());
        }
        for variable in descriptor.environment {
            if let Some(existing) = projection
                .environment
                .insert(variable.name.to_string(), variable.value.to_string())
                && existing != variable.value
            {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors synthesize conflicting {} values",
                        variable.name
                    ),
                ));
            }
        }
        for state in descriptor.managed_state {
            if !state.sandbox_path.starts_with("/home/mez/") {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "managed toolchain state {} must remain beneath /home/mez",
                        state.purpose
                    ),
                ));
            }
            if !managed_paths.insert(state.sandbox_path) {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!(
                        "toolchain descriptors collide in managed state at {}",
                        state.sandbox_path
                    ),
                ));
            }
            projection.managed_state.push(*state);
        }
    }
    Ok(projection)
}

/// Validates one root against its descriptor-owned structural contract.
fn validate_descriptor_root(
    path: &Path,
    descriptor: &ToolchainRootDescriptor,
) -> Result<(), SandboxCompileError> {
    validate_toolchain_root(path, descriptor.label, descriptor.allowed_names)?;
    if !descriptor.allowed_parent_names.is_empty() {
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if !parent.is_some_and(|parent| descriptor.allowed_parent_names.contains(&parent)) {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!(
                    "{} must be directly beneath an allowlisted toolchain root",
                    descriptor.label
                ),
            ));
        }
    }
    if !descriptor.required_executables.is_empty() {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect {}: {error}", descriptor.label),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!("{} must be a real directory", descriptor.label),
            ));
        }
        let canonical = path.canonicalize().map_err(|error| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to canonicalize {}: {error}", descriptor.label),
            )
        })?;
        if canonical != path {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!("{} must use its canonical path", descriptor.label),
            ));
        }
        for relative in descriptor.required_executables {
            validate_distribution_executable(path, relative, descriptor.label)?;
        }
        for relative in descriptor.required_directories {
            validate_distribution_directory(path, relative, descriptor.label)?;
        }
    }
    Ok(())
}

/// Requires one descriptor-owned distribution directory to be real and contained.
fn validate_distribution_directory(
    root: &Path,
    relative: &str,
    label: &str,
) -> Result<(), SandboxCompileError> {
    let directory = root.join(relative);
    let metadata = fs::symlink_metadata(&directory).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect {label} directory {relative}: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} directory {relative} must be a real directory"),
        ));
    }
    Ok(())
}

/// Requires one descriptor-owned executable to remain a real executable file.
fn validate_distribution_executable(
    root: &Path,
    relative: &str,
    label: &str,
) -> Result<(), SandboxCompileError> {
    let executable = root.join(relative);
    let metadata = fs::symlink_metadata(&executable).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect {label} executable {relative}: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} executable {relative} must be a real file"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!("{label} executable {relative} must be executable"),
            ));
        }
    }
    Ok(())
}

/// Canonical host roots for one discovered Rust toolchain projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustToolchainDiscovery {
    /// Canonical Cargo executable directory containing rustup shims.
    pub(crate) cargo_bin: PathBuf,
    /// Canonical Rustup home containing installed toolchains and metadata.
    pub(crate) rustup_home: PathBuf,
}

/// Independently discovered direct-user Rust roots for CLI status output.
///
/// The CLI preserves partial availability so users can see which conventional
/// root is missing. Runtime pane discovery remains all-or-nothing because a
/// sandbox projection requires both roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustToolchainHomeDiscovery {
    /// Canonical Cargo executable directory when it exists and is valid.
    pub(crate) cargo_bin: Option<PathBuf>,
    /// Canonical Rustup home when it exists and is valid.
    pub(crate) rustup_home: Option<PathBuf>,
}

impl RustToolchainHomeDiscovery {
    /// Returns whether both roots required for a Rust projection are present.
    pub(crate) const fn available(&self) -> bool {
        self.cargo_bin.is_some() && self.rustup_home.is_some()
    }
}

/// Parses one stable allowlisted toolchain spelling.
pub(crate) fn parse_sandbox_toolchain_kind(value: &str) -> Option<SandboxToolchainKind> {
    SUPPORTED_SANDBOX_TOOLCHAIN_KINDS
        .into_iter()
        .find(|kind| kind.as_str() == value)
}

/// Discovers the first Zig distribution selected by an explicit search path.
///
/// This direct-user CLI adapter mirrors shell `command -v` ordering without
/// consulting ambient process state, executing manager hooks, or accepting a
/// shim/symlink executable. Missing Zig returns `None`; an invalid selected
/// executable fails closed.
pub(crate) fn discover_zig_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("zig");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Zig executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Zig executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Zig executable has no distribution root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Zig distribution: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &ZIG_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers the first Go SDK selected by an explicit search path.
///
/// The selected executable must be a real `<sdk>/bin/go` file. Discovery
/// canonicalizes and validates the SDK root without consulting GOPATH, GOBIN,
/// ambient process state, or version-manager hooks.
pub(crate) fn discover_go_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("go");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Go executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Go executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Go executable has no SDK root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Go SDK: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &GO_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers the first Deno runtime selected by an explicit search path.
///
/// The selected executable must be a real file directly beneath its runtime
/// root. Discovery does not consult ambient process state, DENO_DIR, or
/// version-manager hooks and never imports host cache or credential state.
pub(crate) fn discover_deno_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("deno");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Deno executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Deno executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Deno executable has no runtime root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Deno runtime: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &DENO_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers the first Bun distribution selected by an explicit search path.
///
/// The selected executable must be a real `<root>/bin/bun` file. Discovery
/// canonicalizes and validates only that distribution root without consulting
/// ambient BUN_INSTALL state or executing version-manager hooks.
pub(crate) fn discover_bun_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("bun");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Bun executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Bun executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Bun executable has no distribution root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Bun distribution: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &BUN_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers the first Node.js distribution selected by an explicit search path.
///
/// The selected executable must be a real `<root>/bin/node` file. Discovery
/// validates only that distribution root and does not execute nvm, fnm, Volta,
/// asdf, mise, or other manager hooks or import ambient npm configuration.
pub(crate) fn discover_node_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("node");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Node.js executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Node.js executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Node.js executable has no distribution root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Node.js distribution: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &NODE_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers the first selected Python base runtime from an explicit search path.
///
/// The executable must be a real `<root>/bin/python3` file. Discovery never
/// runs activation scripts or manager hooks and does not inspect ambient
/// Python, pip, uv, pyenv, asdf, or mise configuration.
pub(crate) fn discover_python_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("python3");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Python executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Python executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Python executable has no runtime root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Python runtime: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &PYTHON_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one selected JDK from an explicit search path without invoking
/// SDKMAN, asdf, mise, jenv, shell hooks, or manager-owned shims.
pub(crate) fn discover_jdk_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("javac");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected JDK compiler: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected JDK compiler must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected JDK compiler has no SDK root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected JDK: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &JDK_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one selected .NET SDK from an explicit search path without
/// invoking asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_dotnet_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("dotnet");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected .NET host: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected .NET host must be a real file, not a shim or symlink",
            ));
        }
        let root = executable.parent().ok_or_else(|| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "selected .NET host has no SDK root",
            )
        })?;
        let root = root.canonicalize().map_err(|error| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to canonicalize selected .NET SDK: {error}"),
            )
        })?;
        validate_descriptor_root(&root, &DOTNET_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one selected Dart SDK from an explicit search path without
/// invoking asdf, mise, Flutter, shell hooks, or manager-owned shims.
pub(crate) fn discover_dart_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("dart");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Dart executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Dart executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Dart executable has no SDK root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Dart SDK: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &DART_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one standalone Kotlin/JVM compiler from an explicit search path
/// without invoking SDKMAN, asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_kotlin_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("kotlinc");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Kotlin compiler: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Kotlin compiler must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Kotlin compiler has no distribution root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Kotlin distribution: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &KOTLIN_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one selected Ruby runtime from an explicit search path without
/// invoking rbenv, RVM, asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_ruby_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join("ruby");
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected Ruby executable: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "selected Ruby executable must be a real file, not a shim or symlink",
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    "selected Ruby executable has no runtime root",
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected Ruby runtime: {error}"),
                )
            })?;
        validate_descriptor_root(&root, &RUBY_ROOTS[0])?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Discovers one selected PHP runtime from an explicit search path without
/// invoking asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_php_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "php",
        "PHP executable",
        "PHP runtime",
        &PHP_ROOTS[0],
    )
}

/// Discovers one selected Composer companion from an explicit search path
/// without reading host Composer configuration or invoking manager hooks.
pub(crate) fn discover_composer_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "composer",
        "Composer executable",
        "Composer companion",
        &COMPOSER_ROOTS[0],
    )
}

/// Discovers one selected Erlang/OTP runtime from an explicit search path
/// without invoking asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_erlang_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "erl",
        "Erlang executable",
        "Erlang/OTP runtime",
        &ERLANG_ROOTS[0],
    )
}

/// Discovers one selected Elixir runtime from an explicit search path without
/// reading host Hex/Rebar state or invoking manager hooks.
pub(crate) fn discover_elixir_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "elixir",
        "Elixir executable",
        "Elixir runtime",
        &ELIXIR_ROOTS[0],
    )
}

/// Discovers one selected GHC compiler from an explicit search path without
/// invoking GHCup, Stack, asdf, mise, shell hooks, or manager-owned shims.
pub(crate) fn discover_ghc_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "ghc",
        "GHC executable",
        "GHC compiler",
        &GHC_ROOTS[0],
    )
}

/// Discovers one selected Cabal companion without reading host package state.
pub(crate) fn discover_cabal_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "cabal",
        "Cabal executable",
        "Cabal companion",
        &CABAL_ROOTS[0],
    )
}

/// Discovers one selected Stack companion without invoking manager hooks.
pub(crate) fn discover_stack_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "stack",
        "Stack executable",
        "Stack companion",
        &STACK_ROOTS[0],
    )
}

/// Discovers one standalone LLVM/Clang toolchain from an explicit search path.
pub(crate) fn discover_llvm_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "clang",
        "Clang executable",
        "LLVM/Clang toolchain",
        &LLVM_ROOTS[0],
    )
}

/// Discovers one standalone GCC toolchain from an explicit search path.
pub(crate) fn discover_gcc_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "gcc",
        "GCC executable",
        "GCC toolchain",
        &GCC_ROOTS[0],
    )
}

/// Discovers one standalone CMake distribution from an explicit search path.
pub(crate) fn discover_cmake_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "cmake",
        "CMake executable",
        "CMake distribution",
        &CMAKE_ROOTS[0],
    )
}

/// Discovers one standalone Ninja distribution from an explicit search path.
pub(crate) fn discover_ninja_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "ninja",
        "Ninja executable",
        "Ninja distribution",
        &NINJA_ROOTS[0],
    )
}

/// Discovers one standalone Meson distribution from an explicit search path.
pub(crate) fn discover_meson_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "meson",
        "Meson executable",
        "Meson distribution",
        &MESON_ROOTS[0],
    )
}

/// Discovers one standalone Swift Linux toolchain from an explicit search path.
///
/// Discovery accepts only a real `<root>/bin/swiftc` executable and validates
/// the complete distribution without invoking swiftenv, asdf, mise, or hooks.
pub(crate) fn discover_swift_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "swiftc",
        "Swift compiler",
        "Swift Linux toolchain",
        &SWIFT_ROOTS[0],
    )
}

/// Discovers one standalone Maven distribution from an explicit search path.
pub(crate) fn discover_maven_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "mvn",
        "Maven executable",
        "Maven distribution",
        &MAVEN_ROOTS[0],
    )
}

/// Discovers one standalone Gradle distribution from an explicit search path.
pub(crate) fn discover_gradle_from_search_path(
    search_path: Option<&OsStr>,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    discover_single_executable_root(
        search_path,
        "gradle",
        "Gradle executable",
        "Gradle distribution",
        &GRADLE_ROOTS[0],
    )
}

/// Discovers a real `<root>/bin/<executable>` and validates its descriptor root.
fn discover_single_executable_root(
    search_path: Option<&OsStr>,
    executable_name: &str,
    executable_label: &str,
    root_label: &str,
    descriptor: &ToolchainRootDescriptor,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let Some(search_path) = search_path else {
        return Ok(None);
    };
    for directory in std::env::split_paths(search_path) {
        let executable = directory.join(executable_name);
        let metadata = match fs::symlink_metadata(&executable) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to inspect selected {executable_label}: {error}"),
                ));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!("selected {executable_label} must be a real file, not a shim or symlink"),
            ));
        }
        let root = executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("selected {executable_label} has no root"),
                )
            })?
            .canonicalize()
            .map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("failed to canonicalize selected {root_label}: {error}"),
                )
            })?;
        validate_descriptor_root(&root, descriptor)?;
        return Ok(Some(root));
    }
    Ok(None)
}

/// Resolves an optional `.venv` contained by one trusted canonical project.
fn resolve_python_project_environment(
    project_root: &Path,
) -> Result<Option<ResolvedProjectEnvironment>, SandboxCompileError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize trusted project root: {error}"),
        )
    })?;
    let environment = project_root.join(".venv");
    match fs::symlink_metadata(&environment) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Python project environment must not be a symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect Python project environment: {error}"),
            ));
        }
    }
    let environment = environment.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize Python project environment: {error}"),
        )
    })?;
    if !environment.starts_with(&project_root)
        || environment.parent() != Some(project_root.as_path())
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Python project environment must remain directly inside the trusted project",
        ));
    }
    validate_python_project_environment(&environment)?;
    Ok(Some(ResolvedProjectEnvironment {
        kind: SandboxToolchainKind::Python,
        sandbox_path: environment.display().to_string(),
        path_entry: Some(format!("{}/bin", environment.display())),
        host_path: environment,
    }))
}

/// Validates one contained Python virtual environment without executing it.
fn validate_python_project_environment(environment: &Path) -> Result<(), SandboxCompileError> {
    let metadata = fs::symlink_metadata(environment).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect Python project environment: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Python project environment must be a real directory",
        ));
    }
    validate_distribution_directory(environment, "bin", "Python project environment")?;
    validate_distribution_executable(environment, "bin/python", "Python project environment")?;
    let config = environment.join("pyvenv.cfg");
    let config_metadata = fs::symlink_metadata(&config).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect Python project environment pyvenv.cfg: {error}"),
        )
    })?;
    if config_metadata.file_type().is_symlink() || !config_metadata.is_file() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Python project environment pyvenv.cfg must be a real file",
        ));
    }
    Ok(())
}

/// Discovers one repository-contained `_opam` local switch without executing
/// `opam env` or consulting global opam configuration and manager state.
pub(crate) fn discover_ocaml_project_environment(
    project_root: &Path,
) -> Result<Option<ResolvedProjectEnvironment>, SandboxCompileError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize trusted project root: {error}"),
        )
    })?;
    let environment = project_root.join("_opam");
    match fs::symlink_metadata(&environment) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "OCaml local switch must not be a symlink",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect OCaml local switch: {error}"),
            ));
        }
    }
    let environment = environment.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize OCaml local switch: {error}"),
        )
    })?;
    if !environment.starts_with(&project_root)
        || environment.parent() != Some(project_root.as_path())
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "OCaml local switch must remain directly inside the trusted project",
        ));
    }
    validate_ocaml_project_environment(&environment)?;
    Ok(Some(ResolvedProjectEnvironment {
        kind: SandboxToolchainKind::Ocaml,
        sandbox_path: environment.display().to_string(),
        path_entry: Some(format!("{}/bin", environment.display())),
        host_path: environment,
    }))
}

/// Validates one repository-contained OCaml switch without executing tools.
fn validate_ocaml_project_environment(environment: &Path) -> Result<(), SandboxCompileError> {
    let metadata = fs::symlink_metadata(environment).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect OCaml local switch: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "OCaml local switch must be a real directory",
        ));
    }
    for directory in ["bin", "lib", "share"] {
        validate_distribution_directory(environment, directory, "OCaml local switch")?;
    }
    for executable in ["bin/ocaml", "bin/ocamlc", "bin/ocamlopt", "bin/dune"] {
        validate_distribution_executable(environment, executable, "OCaml local switch")?;
    }
    Ok(())
}

/// Discovers one direct repository wrapper for Maven or Gradle without
/// executing its script or consulting user-level build-tool configuration.
pub(crate) fn discover_jvm_project_wrapper(
    project_root: &Path,
    kind: SandboxToolchainKind,
) -> Result<Option<ResolvedProjectEnvironment>, SandboxCompileError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize trusted project root: {error}"),
        )
    })?;
    let wrapper_name = match kind {
        SandboxToolchainKind::Maven => "mvnw",
        SandboxToolchainKind::Gradle => "gradlew",
        _ => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "project wrapper discovery requires Maven or Gradle",
            ));
        }
    };
    let wrapper = project_root.join(wrapper_name);
    match fs::symlink_metadata(&wrapper) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                format!("{wrapper_name} must be a real repository file"),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect {wrapper_name}: {error}"),
            ));
        }
    }
    validate_jvm_project_wrapper(&project_root, kind)?;
    Ok(Some(ResolvedProjectEnvironment {
        kind,
        sandbox_path: project_root.display().to_string(),
        path_entry: None,
        host_path: project_root,
    }))
}

/// Validates one repository wrapper script and its pinned HTTPS distribution
/// metadata without downloading content or granting network access.
fn validate_jvm_project_wrapper(
    project_root: &Path,
    kind: SandboxToolchainKind,
) -> Result<(), SandboxCompileError> {
    let (wrapper_name, metadata_path, label) = match kind {
        SandboxToolchainKind::Maven => (
            "mvnw",
            ".mvn/wrapper/maven-wrapper.properties",
            "Maven wrapper",
        ),
        SandboxToolchainKind::Gradle => (
            "gradlew",
            "gradle/wrapper/gradle-wrapper.properties",
            "Gradle wrapper",
        ),
        _ => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "project wrapper validation requires Maven or Gradle",
            ));
        }
    };
    let metadata = fs::symlink_metadata(project_root).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect trusted project root: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "trusted project root must be a real directory",
        ));
    }
    validate_distribution_executable(project_root, wrapper_name, label)?;
    let properties = project_root.join(metadata_path);
    let properties_metadata = fs::symlink_metadata(&properties).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to inspect {label} metadata: {error}"),
        )
    })?;
    if properties_metadata.file_type().is_symlink() || !properties_metadata.is_file() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} metadata must be a real repository file"),
        ));
    }
    if properties_metadata.len() > 64 * 1024 {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} metadata exceeds the 64 KiB limit"),
        ));
    }
    let contents = fs::read_to_string(&properties).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to read {label} metadata: {error}"),
        )
    })?;
    let mut distribution_url = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some(value) = line.strip_prefix("distributionUrl=")
            && distribution_url.replace(value.trim()).is_some()
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("{label} metadata contains duplicate distributionUrl values"),
            ));
        }
    }
    let distribution_url = distribution_url.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} metadata is missing distributionUrl"),
        )
    })?;
    let distribution_url = distribution_url.replace("\\:", ":").replace("\\/", "/");
    let parsed = reqwest::Url::parse(&distribution_url).map_err(|_| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("{label} distributionUrl must be a valid HTTPS URL"),
        )
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} distributionUrl must be credential-free HTTPS"),
        ));
    }
    Ok(())
}

/// Adds the selected OCaml project environment and fails when no trusted local
/// switch is available, rather than falling back to global opam state.
fn append_required_ocaml_project_environment(
    projection: &mut ResolvedToolchainProjection,
    selected: &BTreeSet<SandboxToolchainKind>,
    trusted_project_root: Option<&Path>,
) -> Result<(), SandboxCompileError> {
    if !selected.contains(&SandboxToolchainKind::Ocaml) {
        return Ok(());
    }
    let project_root = trusted_project_root.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected OCaml toolchain requires a trusted project root",
        )
    })?;
    let environment = discover_ocaml_project_environment(project_root)?.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected OCaml toolchain requires a repository-local _opam switch",
        )
    })?;
    projection.project_environments.push(environment);
    Ok(())
}

/// Discovers Rust roots from explicit active-pane bootstrap records.
///
/// Records for unrelated environment managers are ignored. Rust records must
/// use exactly `cargo-bin:<absolute-path>` and `rustup:<absolute-path>` once
/// each; malformed, missing, or duplicate records fail closed.
pub(crate) fn discover_rust_from_environment_managers(
    environment_managers: &[String],
) -> Result<RustToolchainDiscovery, SandboxCompileError> {
    let cargo_bin = unique_manager_path(environment_managers, "cargo-bin")?.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected Rust toolchain requires a canonical Cargo bin directory from pane bootstrap",
        )
    })?;
    let rustup_home = unique_manager_path(environment_managers, "rustup")?.ok_or_else(|| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::UnsupportedRequirement,
            "selected Rust toolchain requires a canonical Rustup home from pane bootstrap",
        )
    })?;
    RustToolchainDiscovery::validated(cargo_bin, rustup_home)
}

/// Discovers Rust roots from the direct user's home directory without
/// changing configuration or creating any filesystem state.
///
/// Missing roots report unavailable as `None`. Existing roots must be real
/// directories rather than symlinks and are canonicalized before the shared
/// validation boundary.
pub(crate) fn discover_rust_from_home(
    home: Option<&Path>,
) -> Result<RustToolchainHomeDiscovery, SandboxCompileError> {
    let Some(home) = home else {
        return Ok(RustToolchainHomeDiscovery {
            cargo_bin: None,
            rustup_home: None,
        });
    };
    let cargo_bin = canonical_existing_directory(&home.join(".cargo/bin"), "Cargo bin")?;
    let rustup_home = canonical_existing_directory(&home.join(".rustup"), "Rustup home")?;
    if let Some(cargo_bin) = cargo_bin.as_ref() {
        validate_cargo_bin(cargo_bin)?;
    }
    if let Some(rustup_home) = rustup_home.as_ref() {
        validate_toolchain_root(rustup_home, "Rustup home", &[".rustup", "rustup"])?;
    }
    if let (Some(cargo_bin), Some(rustup_home)) = (&cargo_bin, &rustup_home) {
        RustToolchainDiscovery::validated(cargo_bin.clone(), rustup_home.clone())?;
    }
    Ok(RustToolchainHomeDiscovery {
        cargo_bin,
        rustup_home,
    })
}

impl RustToolchainDiscovery {
    /// Validates already-resolved roots without adding filesystem authority.
    pub(super) fn validate(&self) -> Result<(), SandboxCompileError> {
        validate_cargo_bin(&self.cargo_bin)?;
        validate_toolchain_root(&self.rustup_home, "Rustup home", &[".rustup", "rustup"])?;
        if self.cargo_bin == self.rustup_home
            || self.cargo_bin.starts_with(&self.rustup_home)
            || self.rustup_home.starts_with(&self.cargo_bin)
        {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::ForbiddenHostPath,
                "Cargo and Rustup homes must be distinct non-overlapping roots",
            ));
        }
        Ok(())
    }

    fn validated(cargo_bin: PathBuf, rustup_home: PathBuf) -> Result<Self, SandboxCompileError> {
        let discovery = Self {
            cargo_bin,
            rustup_home,
        };
        discovery.validate()?;
        Ok(discovery)
    }
}

fn unique_manager_path(
    environment_managers: &[String],
    kind: &str,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let prefix = format!("{kind}:");
    let mut matched = None;
    for manager in environment_managers {
        if manager == kind || manager.starts_with(&prefix) {
            let Some(path) = manager
                .strip_prefix(&prefix)
                .filter(|path| !path.is_empty())
            else {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("pane bootstrap {kind} record must contain one non-empty path"),
                ));
            };
            if matched.replace(PathBuf::from(path)).is_some() {
                return Err(SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("pane bootstrap contains duplicate {kind} records"),
                ));
            }
        }
    }
    Ok(matched)
}

fn canonical_existing_directory(
    path: &Path,
    label: &str,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("failed to inspect {label}: {error}"),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} must be a real directory, not a symlink"),
        ));
    }
    path.canonicalize().map(Some).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("failed to canonicalize {label}: {error}"),
        )
    })
}

fn validate_toolchain_root(
    path: &Path,
    label: &str,
    allowed_names: &[&str],
) -> Result<(), SandboxCompileError> {
    let rendered = path.to_string_lossy();
    validate_printable_absolute_path(&rendered, label)?;
    let name = path.file_name().and_then(|name| name.to_str());
    if !allowed_names.is_empty() && !name.is_some_and(|name| allowed_names.contains(&name)) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} must use an allowlisted toolchain directory name"),
        ));
    }
    if rendered == "/"
        || rendered == "/home"
        || path_overlaps(&rendered, "/run/user")
        || path_overlaps(&rendered, "/var/run")
    {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            format!("{label} overlaps a forbidden host path"),
        ));
    }
    Ok(())
}

fn validate_cargo_bin(path: &Path) -> Result<(), SandboxCompileError> {
    validate_toolchain_root(path, "Cargo bin", &["bin"])?;
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    if !matches!(parent, Some(".cargo" | "cargo")) {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::ForbiddenHostPath,
            "Cargo bin must be directly beneath an allowlisted Cargo home",
        ));
    }
    Ok(())
}
