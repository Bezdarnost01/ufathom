//! Strings whose presence gives a lower bound on the engine version.
//!
//! A marker is sound only when the string can exist in the binary only if
//! the engine feature exists, and the feature appeared in a known version.
//! Then its presence proves `engine >= minimum`.
//!
//! Absence proves nothing, except where a marker's own comment says so:
//! strings can be stripped, packed, or stored in a way the scan does not see.
//! Nothing here sets an upper bound for that reason.
//!
//! The list is short on purpose. A marker whose introduction version is not
//! certain would turn a guess into a claim, so it is not here.

/// A string that implies a minimum engine version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    /// The string, as it appears in the image (ASCII or UTF-16).
    pub name: &'static str,
    /// The `(major, minor)` of the first engine version that can contain it.
    pub minimum: (u16, u16),
    /// Why presence implies the minimum.
    pub reason: &'static str,
}

/// The markers searched for.
pub const MARKERS: &[Marker] = &[
    // `FFieldPathProperty` and the `FieldPathProperty` name that identifies
    // its property class came with the move from `UProperty` objects to
    // `FField` in 4.25, together with `TFieldPath`. Before 4.25 no such
    // property class exists, so the engine has no reason to carry its name.
    // The match is a whole word, so `FieldPathPropertyFoo` or `UFieldPathProperty`
    // do not count, but a path such as `/Script/CoreUObject.FieldPathProperty`
    // does, because it names the same class.
    Marker {
        name: "FieldPathProperty",
        minimum: (4, 25),
        reason: "the FieldPathProperty class was added with FField in 4.25",
    },
];
