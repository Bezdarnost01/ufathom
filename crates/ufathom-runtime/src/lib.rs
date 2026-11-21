//! Library used by generated SDKs: containers, object access and function calls.
//!
//! Everything in this crate is plain data plus checked reads. The types are
//! views over the memory layout of a 64-bit Unreal Engine build, and a
//! [`Session`] gives names, object paths, lookups, `IsA`, class default objects,
//! `GWorld` and `GEngine` through any reader.
//!
//! Calling a game function goes through the [`Executor`] trait. The only
//! implementation that runs game code is `inproc::InProcessExecutor`, behind
//! the `inproc` feature; without it this crate never executes game code.
//!
//! With the `inproc` feature, a session over `SelfMemory` can also write fields
//! of live objects (`Session::live_object`, `LiveObject`). It is the one write
//! path of the crate and it checks the object before every write; external
//! sessions cannot write.
//!
//! # `Pod`
//!
//! [`Pod`] is re-exported from `ufathom-mem`. A trait of our own is not
//! needed: generated types implement the `ufathom-mem` trait with
//! `unsafe impl`, which the orphan rule allows because the type is local to
//! the generated crate. Having one `Pod` also means every generated type works
//! with `MemoryReaderExt::read_pod` directly.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod companions;
mod containers;
mod desc;
#[cfg(all(feature = "detour", windows, target_arch = "x86_64"))]
pub mod detour;
pub mod engine_mem;
mod executor;
#[cfg(feature = "inproc")]
pub mod inproc;
mod name;
mod object;
mod owned;
#[cfg(all(
    windows,
    any(
        feature = "proxy-version",
        feature = "proxy-dsound",
        feature = "proxy-dinput8",
        feature = "proxy-winmm"
    )
))]
pub mod proxy;
mod ptr;
mod session;
mod traits;
mod ubool;
#[cfg(feature = "inproc")]
mod write;

pub use containers::{
    FLazyObjectPtr, FMulticastInlineDelegate, FMulticastSparseDelegate, FScriptDelegate,
    FScriptInterface, FSoftObjectPath, FSoftObjectPtr, FString, FText, FWeakObjectPtr, TArray,
    TMap, TSet,
};
pub use desc::{
    EngineDesc, FNameDesc, NameArrayDesc, NamePoolDesc, NamesDesc, ObjectArrayDesc, ObjectDesc,
    Place, Transform,
};
pub use executor::{CallError, Executor};
pub use name::{FName4, FName8, FName12, FName12Ue5, FNameBytes};
pub use object::{ObjectRef, UObjectBase};
pub use owned::{FStringBuf, TArrayBuf};
pub use ptr::Ptr;
pub use session::{Session, SessionError, SessionResult};
pub use traits::{BitRef, FieldRef, IsA, TypePath};
pub use ubool::UBool;
#[cfg(feature = "inproc")]
pub use write::{LiveObject, WriteError};

/// The memory access crate, re-exported so generated SDKs need no second
/// dependency to read through a `mem::Snapshot` or another reader.
pub use ufathom_mem as mem;
pub use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, Pod};

/// Returns a value with every byte zero.
///
/// This is valid for any [`Pod`] type because every bit pattern is valid.
#[must_use]
pub fn zeroed<T: Pod>() -> T {
    // SAFETY: `T: Pod` is valid for every bit pattern, including all zeroes.
    unsafe { core::mem::zeroed() }
}

/// Views a [`Pod`] value as its bytes, for handing a parameter block to an
/// [`Executor`].
///
/// Generated parameter structs have explicit padding and no implicit padding
/// bytes, so every byte of the view is initialised.
pub fn as_bytes_mut<T: Pod>(value: &mut T) -> &mut [u8] {
    // SAFETY: the pointer is valid for `size_of::<T>()` bytes and uniquely
    // borrowed for the returned lifetime; `T: Pod` makes any byte written valid.
    unsafe {
        core::slice::from_raw_parts_mut(core::ptr::from_mut(value).cast::<u8>(), size_of::<T>())
    }
}
