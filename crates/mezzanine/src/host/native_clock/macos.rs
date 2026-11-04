//! Darwin suspend-inclusive clock with a captured, checked native timebase.

/// Public mach_timebase_info_data_t layout from mach/mach_time.h.
#[repr(C)]
#[derive(Debug)]
pub(super) struct Clock {
    numer: u32,
    denom: u32,
}
unsafe extern "C" {
    /// Public suspend-inclusive native counter, available since macOS 10.12.
    fn mach_continuous_time() -> u64;
    /// Fills the exact public timebase structure; zero return means success.
    #[link_name = "mach_timebase_info"]
    fn native_timebase(info: *mut Clock) -> libc::c_int;
}
impl Clock {
    /// Captures the fixed native ratio before this clock can supply samples.
    pub(super) fn capture() -> Option<Self> {
        let mut clock = Self { numer: 0, denom: 0 };
        // SAFETY: public ABI and initialized correctly sized writable structure.
        if unsafe { native_timebase(&mut clock) } != 0 {
            return None;
        }
        clock.millis()?;
        Some(clock)
    }
    /// Reads the public continuous counter without wall-clock or Instant fallback.
    pub(super) fn millis(&self) -> Option<u64> {
        // SAFETY: public macOS >=10.12 zero-argument continuous-time API.
        super::tick_millis(unsafe { mach_continuous_time() }, self.numer, self.denom)
    }
}
