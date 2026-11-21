//! Field chains: reading `object.root_component()?.relative_location()?`.
//!
//! The generated SDK adds one extension trait per class and struct with an
//! accessor for each field (see the `accessors` feature of the generated
//! crate). Those traits are built on the three items here: [`ChainError`], the
//! error a chain reports, and [`AsObject`] and `AsLive`, the handles the
//! traits are implemented for.
//!
//! A chain is a series of checked reads, one per `?`. It is as fallible as
//! the reads it is made of: every link reads through the reader, so a wrong
//! address or a pointer to unmapped memory is an error, never a fault. A
//! null pointer in the middle is [`ChainError::Null`], which names the field
//! that was null, so the failing link is visible without a debugger.
//!
//! Reads do not check that an object is alive. A stale pointer to memory that
//! still holds readable bytes yields stale values, exactly as
//! [`ObjectRef::get`] does; the reads cannot fault either way. Writes
//! (`LiveObject`) check liveness on every write.

use ufathom_mem::MemError;

use crate::object::ObjectRef;

/// Why a link of a field chain failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChainError {
    /// An object pointer in the chain was null. `field` is the field that
    /// held it, as `Class::Field`, so the error says where the chain broke.
    #[error("null object pointer in field {field}")]
    Null {
        /// The field that was null: the generated type and the field name.
        field: &'static str,
    },
    /// A read failed: unmapped or unreadable memory, or an address overflow.
    #[error(transparent)]
    Memory(#[from] MemError),
    /// A write through a live object was refused.
    #[cfg(feature = "inproc")]
    #[error(transparent)]
    Write(#[from] crate::write::WriteError),
}

/// A read-only handle on an object of a statically known class.
///
/// Implemented by [`ObjectRef`]. Generated accessor traits use it as their
/// base: they are implemented for every handle whose class derives from the
/// class that declares the field, so an inherited field is reachable through a
/// derived object.
pub trait AsObject<'m>: Copy {
    /// The static class of the object.
    type Class: 'static;

    /// The object as an [`ObjectRef`].
    fn object(&self) -> ObjectRef<'m, Self::Class>;
}

impl<'m, T: 'static> AsObject<'m> for ObjectRef<'m, T> {
    type Class = T;

    fn object(&self) -> ObjectRef<'m, T> {
        *self
    }
}

/// A handle on a live object of a statically known class that can be
/// written. Implemented by [`LiveObject`](crate::LiveObject); the generated
/// write accessors are built on it, as the read accessors are on [`AsObject`].
#[cfg(feature = "inproc")]
pub trait AsLive<'s>: Copy {
    /// The static class of the object.
    type Class: 'static;

    /// The object as a [`LiveObject`](crate::LiveObject).
    fn live(&self) -> crate::write::LiveObject<'s, Self::Class>;
}

#[cfg(feature = "inproc")]
impl<'s, T: 'static> AsLive<'s> for crate::write::LiveObject<'s, T> {
    type Class = T;

    fn live(&self) -> crate::write::LiveObject<'s, T> {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_error_names_the_field() {
        let error = ChainError::Null {
            field: "AActor::RootComponent",
        };
        assert_eq!(
            error.to_string(),
            "null object pointer in field AActor::RootComponent"
        );
    }

    #[test]
    fn memory_error_converts() {
        let error: ChainError = MemError::AddressOverflow { addr: 1, len: 2 }.into();
        assert!(matches!(error, ChainError::Memory(_)));
    }
}
