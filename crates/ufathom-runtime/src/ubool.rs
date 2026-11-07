//! Storage for a non-bitfield `bool` property.

use core::fmt;

use ufathom_mem::Pod;

/// One byte holding a `bool` as the engine stores it.
///
/// A Rust `bool` is only valid for the bit patterns 0 and 1, so it must never
/// be read straight from target memory. `UBool` is valid for every byte and
/// converts on access: any non-zero byte reads as `true`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct UBool(pub u8);

impl UBool {
    /// The stored value as a `bool`. Any non-zero byte is `true`.
    #[must_use]
    pub const fn get(self) -> bool {
        self.0 != 0
    }

    /// Stores `value` as 0 or 1.
    pub const fn set(&mut self, value: bool) {
        self.0 = value as u8;
    }
}

impl From<bool> for UBool {
    fn from(value: bool) -> Self {
        Self(u8::from(value))
    }
}

impl From<UBool> for bool {
    fn from(value: UBool) -> Self {
        value.get()
    }
}

impl fmt::Debug for UBool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 <= 1 {
            fmt::Debug::fmt(&self.get(), f)
        } else {
            write!(f, "UBool({:#04x})", self.0)
        }
    }
}

// SAFETY: a transparent `u8`, valid for every bit pattern.
unsafe impl Pod for UBool {}

const _: () = assert!(size_of::<UBool>() == 1 && align_of::<UBool>() == 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_nonzero_byte_is_true() {
        assert!(!UBool(0).get());
        assert!(UBool(1).get());
        assert!(UBool(0x80).get());
        let mut b = UBool(7);
        b.set(false);
        assert_eq!(b.0, 0);
        b.set(true);
        assert_eq!(b.0, 1);
    }
}
