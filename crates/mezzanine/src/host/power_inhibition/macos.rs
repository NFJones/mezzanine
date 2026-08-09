//! Native macOS IOKit power-assertion adapter.
//!
//! Unsafe FFI remains in this module. Each request creates a short-lived Core
//! Foundation UTF-8 string for the assertion type and reason, then transfers
//! only the numeric IOKit assertion id to the platform-neutral controller.

use std::ffi::c_void;
use std::ptr;

use super::{PowerInhibitionBackend, PowerInhibitionResource};

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
const K_IO_RETURN_SUCCESS: i32 = 0;
const ASSERTION_REASON: &str = "Mezzanine is running an active agent turn";

/// Returns the native IOKit assertion-type string for one requested resource.
fn assertion_type(resource: PowerInhibitionResource) -> &'static str {
    match resource {
        PowerInhibitionResource::System => "PreventUserIdleSystemSleep",
        PowerInhibitionResource::Display => "PreventUserIdleDisplaySleep",
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external_representation: bool,
    ) -> *const c_void;
    fn CFRelease(value: *const c_void);
}

/// Native IOKit calls required by the macOS power-inhibition adapter.
trait MacOsPowerInhibitionApi {
    /// Creates one named IOKit assertion for the requested resource.
    fn acquire(&mut self, resource: PowerInhibitionResource) -> std::result::Result<u32, String>;
    /// Releases one assertion previously created by this adapter.
    fn release(&mut self, lease_id: u32) -> std::result::Result<(), String>;
}

/// Production implementation of the native macOS assertion calls.
#[derive(Debug, Default)]
pub(crate) struct NativeMacOsPowerInhibitionApi;

impl MacOsPowerInhibitionApi for NativeMacOsPowerInhibitionApi {
    fn acquire(&mut self, resource: PowerInhibitionResource) -> std::result::Result<u32, String> {
        let assertion_type = cf_string(assertion_type(resource))?;
        let assertion_name = cf_string(ASSERTION_REASON)?;
        let mut assertion_id = 0;
        // SAFETY: Both strings are valid retained CFString references for the
        // duration of this call, and assertion_id points to writable storage.
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.0,
                K_IOPM_ASSERTION_LEVEL_ON,
                assertion_name.0,
                &mut assertion_id,
            )
        };
        if result == K_IO_RETURN_SUCCESS {
            Ok(assertion_id)
        } else {
            Err(format!(
                "IOPMAssertionCreateWithName failed with IOReturn {result}"
            ))
        }
    }

    fn release(&mut self, lease_id: u32) -> std::result::Result<(), String> {
        // SAFETY: lease_id was returned by IOPMAssertionCreateWithName and is
        // released only by the owning transition controller.
        let result = unsafe { IOPMAssertionRelease(lease_id) };
        if result == K_IO_RETURN_SUCCESS {
            Ok(())
        } else {
            Err(format!(
                "IOPMAssertionRelease failed with IOReturn {result}"
            ))
        }
    }
}

/// IOKit-backed host power-inhibition implementation for macOS.
///
/// The generic API boundary lets deterministic tests verify requested resource
/// types and cleanup without creating real host power assertions.
#[derive(Debug, Default)]
pub(crate) struct MacOsPowerInhibitionBackend<A = NativeMacOsPowerInhibitionApi> {
    api: A,
}

impl MacOsPowerInhibitionBackend {
    /// Creates the production macOS power-inhibition backend.
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl<A> MacOsPowerInhibitionBackend<A> {
    /// Creates a macOS backend backed by deterministic native-call test data.
    fn with_api(api: A) -> Self {
        Self { api }
    }
}

impl<A: MacOsPowerInhibitionApi> PowerInhibitionBackend for MacOsPowerInhibitionBackend<A> {
    fn acquire(&mut self, resource: PowerInhibitionResource) -> std::result::Result<u32, String> {
        self.api.acquire(resource)
    }

    fn release(&mut self, lease_id: u32) -> std::result::Result<(), String> {
        self.api.release(lease_id)
    }
}

/// Owned Core Foundation string released when it leaves scope.
struct OwnedCfString(*const c_void);

impl Drop for OwnedCfString {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the non-null reference returned by
        // CFStringCreateWithBytes and releases it exactly once.
        unsafe { CFRelease(self.0) };
    }
}

/// Converts a static UTF-8 string into a retained Core Foundation string.
fn cf_string(value: &str) -> std::result::Result<OwnedCfString, String> {
    let length =
        isize::try_from(value.len()).map_err(|_| "CFString input is too long".to_string())?;
    // SAFETY: value bytes are valid for this synchronous call and the null
    // allocator requests Core Foundation's default allocator.
    let string = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            value.as_ptr(),
            length,
            K_CF_STRING_ENCODING_UTF8,
            false,
        )
    };
    if string.is_null() {
        Err("CFStringCreateWithBytes returned null".to_string())
    } else {
        Ok(OwnedCfString(string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct FakeMacOsApi {
        calls: Vec<String>,
        next_id: u32,
    }

    impl MacOsPowerInhibitionApi for FakeMacOsApi {
        fn acquire(
            &mut self,
            resource: PowerInhibitionResource,
        ) -> std::result::Result<u32, String> {
            self.calls.push(format!(
                "acquire:{}:{}",
                assertion_type(resource),
                ASSERTION_REASON
            ));
            self.next_id += 1;
            Ok(self.next_id)
        }

        fn release(&mut self, lease_id: u32) -> std::result::Result<(), String> {
            self.calls.push(format!("release:{lease_id}"));
            Ok(())
        }
    }

    /// Verifies the macOS adapter selects the documented IOKit assertion types
    /// and stable diagnostic reason without changing the host power state.
    #[test]
    fn macos_assertion_resources_use_documented_iokit_types() {
        assert_eq!(
            assertion_type(PowerInhibitionResource::System),
            "PreventUserIdleSystemSleep"
        );
        assert_eq!(
            assertion_type(PowerInhibitionResource::Display),
            "PreventUserIdleDisplaySleep"
        );
        assert_eq!(
            ASSERTION_REASON,
            "Mezzanine is running an active agent turn"
        );
    }

    /// Verifies the macOS backend delegates exact resource and reason data to
    /// its injected native API and releases the returned assertion identifier.
    #[test]
    fn macos_backend_delegates_assertions_without_host_power_changes() {
        let mut backend = MacOsPowerInhibitionBackend::with_api(FakeMacOsApi::default());

        let system = backend.acquire(PowerInhibitionResource::System).unwrap();
        let display = backend.acquire(PowerInhibitionResource::Display).unwrap();
        backend.release(display).unwrap();
        backend.release(system).unwrap();

        assert_eq!(
            backend.api.calls,
            [
                "acquire:PreventUserIdleSystemSleep:Mezzanine is running an active agent turn",
                "acquire:PreventUserIdleDisplaySleep:Mezzanine is running an active agent turn",
                "release:2",
                "release:1",
            ]
        );
    }
}
