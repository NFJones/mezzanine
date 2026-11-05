//! Allocation-bound regression for the quarantine-only source reader.
//!
//! This binary alone observes its test thread's allocation requests. Read
//! generates the source body without a body-sized fixture allocation. The full
//! decoder is a positive control; the descriptor must validate and hash a larger
//! body using only fixed read/UTF-8 buffers and bounded metadata allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{self, Read};
use std::sync::atomic::{AtomicUsize, Ordering};

use mez_agent::messaging::{
    MessageAcceptanceDecodeLimits, decode_message_acceptance, decode_message_acceptance_metadata,
};
use sha2::{Digest, Sha256};

struct Allocator;
thread_local! { static TRACK: Cell<bool> = const { Cell::new(false) }; }
static MAX_REQUEST: AtomicUsize = AtomicUsize::new(0);

fn observe(bytes: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        MAX_REQUEST.fetch_max(bytes, Ordering::Relaxed);
    }
}

// SAFETY: every operation delegates unchanged to System. Observing atomic
// sizes neither dereferences allocations nor recursively allocates.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        observe(bytes);
        unsafe { System.realloc(pointer, layout, bytes) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct Tracking;
impl Tracking {
    fn begin() -> Self {
        MAX_REQUEST.store(0, Ordering::Relaxed);
        TRACK.with(|flag| flag.set(true));
        Self
    }
}
impl Drop for Tracking {
    fn drop(&mut self) {
        TRACK.with(|flag| flag.set(false));
    }
}

fn source(bytes: usize) -> impl Read {
    const PREFIX: &[u8] = br#"{"schema_version":1,"sequence":1,"accepted_at_ms":1,"sender_registered_after_sequence":0,"envelope":{"protocol":"mmp/1","id":"original","message_type":"send","time":"original","sender":{"agent_id":"sender","pane_id":null,"window_id":null,"role":null,"capabilities":[],"objective":null,"project_scope":null},"recipient":{"kind":"agent","value":"target"},"correlation_id":null,"ttl_ms":5,"content_type":"text/plain","payload":""#;
    const SUFFIX: &[u8] = br#"","extension_fields":[]},"audience":{"kind":"session","project_scope":null},"recipient_bindings":[{"recipient":"target","registered_after_sequence":0}]}"#;
    PREFIX
        .chain(io::repeat(b'x').take(bytes as u64))
        .chain(SUFFIX)
}

fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 4 * 1024 * 1024,
        auxiliary_bytes: 4096,
        recipient_count: 1,
        recipient_bytes: 256,
    }
}

/// A descriptor validates four MiB with no heap request above the fixed 8-KiB
/// reader buffer. The full decoder's one-MiB body must be observed, proving the
/// meter detects an accidental retain-then-hash implementation.
#[test]
fn metadata_payload_validation_has_no_payload_sized_allocation() {
    let tracking = Tracking::begin();
    let full = decode_message_acceptance(source(1024 * 1024), limits()).unwrap();
    drop(tracking);
    assert_eq!(full.envelope().payload.len(), 1024 * 1024);
    assert!(MAX_REQUEST.load(Ordering::Relaxed) >= 1024 * 1024);
    drop(full);

    let tracking = Tracking::begin();
    let descriptor = decode_message_acceptance_metadata(source(4 * 1024 * 1024), limits()).unwrap();
    drop(tracking);
    let largest = MAX_REQUEST.load(Ordering::Relaxed);
    assert!(
        largest <= 8192,
        "metadata decoder requested {largest} bytes"
    );
    assert_eq!(descriptor.envelope().payload.bytes(), 4 * 1024 * 1024);
    let mut hash = Sha256::new();
    for _ in 0..512 {
        hash.update([b'x'; 8192]);
    }
    assert_eq!(
        descriptor.envelope().payload.sha256(),
        &<[u8; 32]>::from(hash.finalize())
    );
}
