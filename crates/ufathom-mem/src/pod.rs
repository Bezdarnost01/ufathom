//! Plain-old-data marker for types that can be read straight from bytes.

/// Types that are valid for every possible bit pattern.
///
/// This lets a value be built by copying bytes read from a target.
///
/// # Safety
///
/// Implementors must be `Copy` and every bit pattern of `size_of::<Self>()`
/// bytes must be a valid value of the type. Padding bytes are allowed because
/// they are overwritten by the copy and never inspected. `bool`, `char`,
/// enums, references, pointers with validity rules, `NonZero*` and any type
/// with a niche must **not** implement this trait.
pub unsafe trait Pod: Copy + 'static {}

macro_rules! impl_pod {
    ($($ty:ty),* $(,)?) => {
        $(
            // SAFETY: primitive numeric types accept every bit pattern and have no padding.
            unsafe impl Pod for $ty {}
        )*
    };
}

impl_pod!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize, f32, f64
);

// SAFETY: an array of `Pod` elements is a run of `Pod` values with no padding between them.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}
