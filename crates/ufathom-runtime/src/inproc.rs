//! [`InProcessExecutor`]: calls game functions through `UObject::ProcessEvent`.
//!
//! Only compiled with the `inproc` feature. This is the one place in the
//! runtime that runs game code.

use core::ffi::c_void;
use core::marker::PhantomData;

use ufathom_mem::{MemoryReaderExt, SelfMemory};

use crate::desc::EngineDesc;
use crate::executor::{CallError, Executor};
use crate::object::UObjectBase;
use crate::ptr::Ptr;
use crate::session::{Session, SessionError};

/// `void UObject::ProcessEvent(UFunction*, void*)`, a virtual member
/// function. On x86-64 a `this` call uses the platform C convention with
/// `this` as the first argument.
type ProcessEventFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);

/// Calls game functions from inside the game process.
///
/// It owns a [`Session`] over [`SelfMemory`], so it also gives the whole
/// lookup toolbox through [`session`](Self::session). Each call:
///
/// 1. checks that the object is non-null, readable and still sits in the slot
///    of the object array its `InternalIndex` names;
/// 2. finds the `UFunction` by path (cached, re-validated on every hit) and
///    checks that it is a function;
/// 3. checks that the parameter block is at least as large as the function's
///    `PropertiesSize`;
/// 4. reads `ProcessEvent` from the object's virtual table at the index found
///    by discovery, and calls it.
///
/// The executor is neither `Send` nor `Sync`: it stays on the thread that
/// attached it, which by the contract of [`attach`](Self::attach) is the game
/// thread.
///
/// Errors inside the game function itself (a C++ exception, an access
/// violation, a fatal engine check) are not caught: they behave as if the
/// game had made the call.
pub struct InProcessExecutor {
    session: Session<SelfMemory>,
    process_event: u32,
    _not_send: PhantomData<*const ()>,
}

impl InProcessExecutor {
    /// Attaches to the game this code runs in.
    ///
    /// # Safety
    ///
    /// - The caller runs inside the game process described by `desc`, and
    ///   `desc` describes this exact build.
    /// - Every call through the executor happens on the game thread, or at a
    ///   moment when the game thread cannot run engine code concurrently (for
    ///   example from inside a `ProcessEvent` or tick hook). The executor
    ///   cannot be moved to another thread, so attaching on the game thread
    ///   is the simplest way to keep this promise.
    ///
    /// # Errors
    ///
    /// [`SessionError::Unknown`] when `desc` has no `ProcessEvent` index,
    /// otherwise as [`Session::new`].
    pub unsafe fn attach(desc: &'static EngineDesc) -> Result<Self, SessionError> {
        let process_event = desc
            .process_event_index
            .ok_or(SessionError::Unknown("ProcessEvent vtable index"))?;
        Ok(Self {
            session: Session::new(SelfMemory::new(), desc)?,
            process_event,
            _not_send: PhantomData,
        })
    }

    /// Names, objects and lookups for this game.
    #[must_use]
    pub fn session(&self) -> &Session<SelfMemory> {
        &self.session
    }

    /// The `UFunction` at `path`, checked to be a function.
    fn function(&self, path: &str) -> Result<Ptr<UObjectBase>, CallError> {
        let not_found = || CallError::FunctionNotFound(path.to_owned());
        let function = self
            .session
            .find_object(path)
            .map_err(|error| CallError::Failed(error.to_string()))?
            .ok_or_else(not_found)?;
        let class = self
            .session
            .class_of(function)
            .and_then(|class| self.session.name_of(class))
            .map_err(|error| CallError::Failed(error.to_string()))?;
        if !class.ends_with("Function") {
            return Err(not_found());
        }
        Ok(function)
    }
}

impl core::fmt::Debug for InProcessExecutor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InProcessExecutor")
            .field("process_event", &self.process_event)
            .field("session", &self.session)
            .finish()
    }
}

// SAFETY: `attach` is `unsafe` and its contract (inside the described game,
// on the game thread) is the one this trait requires. Each call validates the
// object and the function and checks the parameter block size before the
// game code runs.
unsafe impl Executor for InProcessExecutor {
    fn process_event(
        &self,
        object: Ptr<UObjectBase>,
        function_path: &str,
        params: &mut [u8],
    ) -> Result<(), CallError> {
        if !self.session.is_valid(object) {
            return Err(CallError::InvalidObject {
                addr: object.addr(),
            });
        }
        let function = self.function(function_path)?;
        let expected = self
            .session
            .params_size(function)
            .map_err(|error| CallError::Failed(error.to_string()))?;
        if params.len() < expected {
            return Err(CallError::ParamsSize {
                expected,
                actual: params.len(),
            });
        }

        let mem = self.session.memory();
        let failed = |what: &str| CallError::Failed(format!("cannot read {what}"));
        let vtable = mem
            .read_u64(object.addr())
            .map_err(|_| failed("the virtual table pointer"))?;
        let slot_at = u64::from(self.process_event)
            .checked_mul(8)
            .and_then(|offset| vtable.checked_add(offset))
            .ok_or_else(|| failed("the ProcessEvent slot"))?;
        let entry = mem
            .read_u64(slot_at)
            .map_err(|_| failed("the ProcessEvent slot"))?;
        let entry = usize::try_from(entry).map_err(|_| failed("the ProcessEvent address"))?;
        let this = usize::try_from(object.addr()).map_err(|_| failed("the object address"))?;
        let function =
            usize::try_from(function.addr()).map_err(|_| failed("the function address"))?;
        if entry == 0 {
            return Err(CallError::Failed("ProcessEvent slot is null".to_owned()));
        }
        let parms = if params.is_empty() {
            core::ptr::null_mut()
        } else {
            params.as_mut_ptr().cast::<c_void>()
        };

        // SAFETY: `entry` is the non-null `ProcessEvent` slot of the object's
        // own virtual table, at the index discovery found for this build
        // (contract of `attach`). Function pointers and data pointers have the
        // same size on the supported targets.
        let process_event = unsafe { core::mem::transmute::<usize, ProcessEventFn>(entry) };
        // SAFETY: the object is live (checked against its object array slot),
        // the function is a `UFunction` found by path, `parms` points to at
        // least `PropertiesSize` writable bytes laid out as the function's
        // parameter struct (contract of `Executor`), and we are on the game
        // thread (contract of `attach`; the executor is `!Send`).
        unsafe {
            process_event(this as *mut c_void, function as *mut c_void, parms);
        }
        Ok(())
    }
}
