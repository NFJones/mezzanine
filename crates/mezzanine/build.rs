//! Compiles the Darwin kernel-credential shim and stamps outbound projection owners.
//!
//! The SDK owns kinfo_proc layout; the shim exports only a fixed private record.
//! Native credential capture runs in-process and never executes build tools.
//! Projection source stamps provide content-free fault
//! attribution. This is a source fingerprint, not an executable attestation or
//! a claim that the working checkout is the running daemon. No Git, runtime
//! configuration, credentials, network or session data is consulted.

use sha2::{Digest, Sha256};

/// Hashes length-delimited owner paths and bytes, rebuilding when any changes.
/// Missing source is a build failure rather than a fabricated build identity.
fn main() -> std::io::Result<()> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let shim = "src/security/filesystem/native_context/darwin_credentials.c";
        println!("cargo:rerun-if-changed={shim}");
        cc::Build::new()
            .file(shim)
            .std("c11")
            .define("_DARWIN_C_SOURCE", None)
            .warnings(true)
            .extra_warnings(true)
            .compile("mez_native_darwin_credentials");
    }
    let mut digest = Sha256::new();
    for path in [
        "build.rs",
        "src/runtime/render/streaming_projection_outcome.rs",
        "src/runtime/render/presentation/buffer_apply/streaming_projection.rs",
        "src/runtime/render/presentation/buffer_apply/outbound_messages.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
        let bytes = std::fs::read(path)?;
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    let hex = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("cargo:rustc-env=MEZ_OUTBOUND_PROJECTION_SHA256={hex}");
    Ok(())
}
