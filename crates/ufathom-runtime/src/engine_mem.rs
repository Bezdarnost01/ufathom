//! Freeing strings and arrays the engine allocated.
//!
//! A game function that returns an `FString` or a `TArray`, or fills an out
//! parameter of that type, allocates the buffer with the engine's allocator.
//! Rust cannot free it with its own. These functions hand the buffer back to
//! the engine through a reflected function, so no allocator address has to be
//! found: `KismetArrayLibrary::Array_Clear` empties the array it is given,
//! which calls `FMemory::Realloc(Data, 0)` on its buffer.
//!
//! `Array_Clear` sees every array as an array of `int32`, which has no
//! destructor. So only the outer buffer is freed: elements that own memory
//! themselves (an array of `FString`, or of structs with strings) need their
//! own buffers freed first, as [`free_strings`] does for an array of strings.
//!
//! Only buffers the engine allocated may be freed. A buffer of an
//! [`FStringBuf`](crate::FStringBuf) or a [`TArrayBuf`](crate::TArrayBuf) is
//! refused.

use ufathom_mem::MemoryReader;

use crate::containers::{FString, TArray};
use crate::executor::{CallError, Executor};
use crate::owned;

/// The class whose default object `Array_Clear` runs on.
pub const ARRAY_LIBRARY: &str = "/Script/Engine.KismetArrayLibrary";
/// `UKismetArrayLibrary::Array_Clear(TArray<int32>& TargetArray)`.
pub const ARRAY_CLEAR: &str = "/Script/Engine.KismetArrayLibrary:Array_Clear";

/// Frees the buffer of an array the engine allocated and leaves `array`
/// empty. An array without a buffer is left as it is.
///
/// The elements are not destroyed: free what they own first.
///
/// # Errors
///
/// [`CallError::Refused`] when the header is inconsistent or the buffer is
/// owned by Rust, the executor's errors when the call fails, and
/// [`CallError::Failed`] when the engine did not free the buffer.
pub fn free_array<T>(
    exec: &(impl Executor + ?Sized),
    array: &mut TArray<T>,
) -> Result<(), CallError> {
    if array.data.is_null() {
        *array = TArray::default();
        return Ok(());
    }
    if array.num < 0 || array.max <= 0 || array.num > array.max {
        return Err(CallError::Refused(format!(
            "array {array:?} is not consistent; it is not freed"
        )));
    }
    let data = array.data.addr();
    if owned::is_rust_owned(data) {
        return Err(CallError::Refused(format!(
            "array data {data:#x} belongs to an FStringBuf or TArrayBuf, not to the engine"
        )));
    }
    let library = exec.default_object(ARRAY_LIBRARY)?;
    // The parameter block is one `TArray`. `Num` is set to at least 1:
    // `EmptyValues` frees the buffer only when the array is not empty.
    let mut block = [0u64; 2];
    block[0] = data;
    let max = u32::try_from(array.max).unwrap_or(0);
    block[1] = (u64::from(max) << 32) | 1;
    exec.process_event(library, ARRAY_CLEAR, crate::as_bytes_mut(&mut block))?;
    if block[0] != 0 {
        return Err(CallError::Failed(format!(
            "{ARRAY_CLEAR} left the buffer at {:#x}",
            block[0]
        )));
    }
    *array = TArray::default();
    Ok(())
}

/// Frees the characters of a string the engine allocated and leaves it
/// empty.
///
/// # Errors
///
/// As [`free_array`].
pub fn free_string(exec: &(impl Executor + ?Sized), string: &mut FString) -> Result<(), CallError> {
    free_array(exec, &mut string.chars)
}

/// Reads a string the engine allocated, then frees it. On a read error the
/// string is still freed.
///
/// # Errors
///
/// As [`free_array`], or [`CallError::Failed`] when the characters cannot
/// be read.
pub fn take_string<M: MemoryReader + ?Sized>(
    exec: &(impl Executor + ?Sized),
    mem: &M,
    string: &mut FString,
) -> Result<String, CallError> {
    let text = string.read(mem);
    free_string(exec, string)?;
    text.map_err(|error| CallError::Failed(format!("cannot read the string: {error}")))
}

/// Frees every string of an array of strings the engine allocated, then the
/// array itself. Each string is freed even when another one fails; the first
/// error is returned and the array is freed only when all strings were.
///
/// # Errors
///
/// As [`free_array`], or [`CallError::Failed`] when the elements cannot be
/// read.
pub fn free_strings<M: MemoryReader + ?Sized>(
    exec: &(impl Executor + ?Sized),
    mem: &M,
    array: &mut TArray<FString>,
) -> Result<(), CallError> {
    if array.data.is_null() {
        *array = TArray::default();
        return Ok(());
    }
    let mut strings = array
        .read_all(mem)
        .map_err(|error| CallError::Failed(format!("cannot read the strings: {error}")))?;
    let mut first_error = None;
    for string in &mut strings {
        if let Err(error) = free_string(exec, string) {
            first_error.get_or_insert(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    free_array(exec, array)
}

/// Takes the strings of an array of strings the engine allocated, then frees
/// them and the array.
///
/// # Errors
///
/// As [`free_strings`].
pub fn take_strings<M: MemoryReader + ?Sized>(
    exec: &(impl Executor + ?Sized),
    mem: &M,
    array: &mut TArray<FString>,
) -> Result<Vec<String>, CallError> {
    let strings = if array.data.is_null() {
        Vec::new()
    } else {
        array
            .read_all(mem)
            .map_err(|error| CallError::Failed(format!("cannot read the strings: {error}")))?
    };
    let texts: Result<Vec<String>, _> = strings.iter().map(|s| s.read(mem)).collect();
    free_strings(exec, mem, array)?;
    texts.map_err(|error| CallError::Failed(format!("cannot read a string: {error}")))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::mem::SelfMemory;
    use crate::object::UObjectBase;
    use crate::ptr::Ptr;
    use crate::{FStringBuf, TArrayBuf};

    const LIBRARY_CDO: u64 = 0x1234_0000;

    /// Records calls and empties the array the way the engine does.
    struct FakeEngine {
        freed: RefCell<Vec<u64>>,
        keep: bool,
    }

    // SAFETY: a test double; it runs no game code.
    unsafe impl Executor for FakeEngine {
        fn process_event(
            &self,
            object: Ptr<UObjectBase>,
            function_path: &str,
            params: &mut [u8],
        ) -> Result<(), CallError> {
            assert_eq!(object.addr(), LIBRARY_CDO);
            assert_eq!(function_path, ARRAY_CLEAR);
            assert_eq!(params.len(), 16);
            let data = u64::from_le_bytes(params[..8].try_into().unwrap());
            let num = i32::from_le_bytes(params[8..12].try_into().unwrap());
            assert!(num >= 1, "Num must be non-zero for EmptyValues to free");
            self.freed.borrow_mut().push(data);
            if !self.keep {
                params.fill(0);
            }
            Ok(())
        }

        fn default_object(&self, class_path: &str) -> Result<Ptr<UObjectBase>, CallError> {
            assert_eq!(class_path, ARRAY_LIBRARY);
            Ok(Ptr::new(LIBRARY_CDO))
        }
    }

    fn engine() -> FakeEngine {
        FakeEngine {
            freed: RefCell::new(Vec::new()),
            keep: false,
        }
    }

    fn engine_array<T>(data: u64, num: i32, max: i32) -> TArray<T> {
        TArray {
            data: Ptr::new(data),
            num,
            max,
        }
    }

    #[test]
    fn an_engine_array_is_freed_and_emptied() {
        let exec = engine();
        let mut array = engine_array::<u32>(0x5000, 0, 8);
        free_array(&exec, &mut array).unwrap();
        assert_eq!(*exec.freed.borrow(), vec![0x5000]);
        assert!(array.data.is_null() && array.num == 0 && array.max == 0);
        // An empty header needs no call.
        free_array(&exec, &mut array).unwrap();
        assert_eq!(exec.freed.borrow().len(), 1);
    }

    #[test]
    fn inconsistent_and_rust_owned_buffers_are_refused() {
        let exec = engine();
        for (num, max) in [(-1, 4), (5, 4), (0, 0)] {
            let mut array = engine_array::<u8>(0x5000, num, max);
            assert!(matches!(
                free_array(&exec, &mut array),
                Err(CallError::Refused(_))
            ));
        }
        let text = FStringBuf::new("mine");
        let mut view = text.as_fstring();
        assert!(matches!(
            free_string(&exec, &mut view),
            Err(CallError::Refused(_))
        ));
        let items = TArrayBuf::new(vec![1u64, 2]);
        let mut view = items.as_tarray();
        assert!(matches!(
            free_array(&exec, &mut view),
            Err(CallError::Refused(_))
        ));
        assert!(exec.freed.borrow().is_empty());
        // Once the Rust buffer is gone, its address is no longer protected.
        let addr = text.as_fstring().chars.data.addr();
        drop(text);
        assert!(!owned::is_rust_owned(addr));
    }

    #[test]
    fn a_buffer_the_engine_kept_is_an_error() {
        let exec = FakeEngine {
            freed: RefCell::new(Vec::new()),
            keep: true,
        };
        let mut array = engine_array::<u8>(0x5000, 1, 1);
        assert!(matches!(
            free_array(&exec, &mut array),
            Err(CallError::Failed(_))
        ));
    }

    #[test]
    fn strings_are_read_then_freed_inner_first() {
        // Engine-shaped strings, in memory this test owns but did not
        // register as Rust buffers.
        let hello: Vec<u16> = "hello\0".encode_utf16().collect();
        let world: Vec<u16> = "world\0".encode_utf16().collect();
        let string = |units: &Vec<u16>| FString {
            chars: engine_array(units.as_ptr() as u64, 6, 6),
        };
        let mut single = string(&hello);
        let exec = engine();
        let mem = SelfMemory::new();
        assert_eq!(take_string(&exec, &mem, &mut single).unwrap(), "hello");
        assert!(single.chars.data.is_null());

        let elements = [string(&hello), string(&world)];
        let mut array = engine_array::<FString>(elements.as_ptr() as u64, 2, 2);
        let exec = engine();
        assert_eq!(
            take_strings(&exec, &mem, &mut array).unwrap(),
            vec!["hello".to_owned(), "world".to_owned()]
        );
        assert_eq!(
            *exec.freed.borrow(),
            vec![
                hello.as_ptr() as u64,
                world.as_ptr() as u64,
                elements.as_ptr() as u64
            ]
        );
        assert!(array.data.is_null());
    }
}
