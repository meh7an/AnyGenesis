//! The vendored ymfm's YM2612 behind a safe handle, for register-script renders: writes to
//! its four ports, its status register, and stereo samples one 144-clock period each.

use std::ffi::c_void;

unsafe extern "C" {
    fn ag_ymfm_new() -> *mut c_void;
    fn ag_ymfm_free(chip: *mut c_void);
    fn ag_ymfm_reset(chip: *mut c_void);
    fn ag_ymfm_write(chip: *mut c_void, offset: u32, data: u8);
    fn ag_ymfm_read_status(chip: *mut c_void) -> u8;
    fn ag_ymfm_generate(chip: *mut c_void, out: *mut i32, count: u32);
}

/// One YM2612, reset.
pub struct Ym2612 {
    chip: *mut c_void,
}

impl Ym2612 {
    pub fn new() -> Self {
        // SAFETY: the shim allocates and resets a chip; it is freed once, in Drop.
        Self {
            chip: unsafe { ag_ymfm_new() },
        }
    }

    /// Resets the chip as ymfm does: its registers and status flags, not the host's timers.
    pub fn reset(&mut self) {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_reset(self.chip) }
    }

    /// Writes `data` to port `offset` (0 address I, 1 data I, 2 address II, 3 data II).
    pub fn write(&mut self, offset: u32, data: u8) {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_write(self.chip, offset, data) }
    }

    /// Writes register `register` of part `part` (0 or 1).
    pub fn set(&mut self, part: u32, register: u8, data: u8) {
        self.write(2 * part, register);
        self.write(2 * part + 1, data);
    }

    pub fn status(&mut self) -> u8 {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_read_status(self.chip) }
    }

    /// Renders `out.len() / 2` stereo samples (left, right interleaved).
    pub fn generate(&mut self, out: &mut [i32]) {
        let count = u32::try_from(out.len() / 2).expect("fewer than 2^32 samples");
        // SAFETY: `out` holds 2 * count values; `chip` is live until Drop.
        unsafe { ag_ymfm_generate(self.chip, out.as_mut_ptr(), count) }
    }
}

impl Default for Ym2612 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Ym2612 {
    fn drop(&mut self) {
        // SAFETY: allocated by ag_ymfm_new, freed only here.
        unsafe { ag_ymfm_free(self.chip) }
    }
}
