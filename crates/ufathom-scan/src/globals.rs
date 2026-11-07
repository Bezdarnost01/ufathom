//! Step 6: `GWorld`, `GEngine` and `ProcessEvent`.
//!
//! - A global slot is a pointer slot in writable module data that holds a
//!   live (in the object array, not a default object) instance of `World`, or
//!   of a class derived from `GameEngine`. When several slots qualify, all are
//!   reported and none is chosen.
//! - `ProcessEvent` is the virtual function of object 0 that tests
//!   `FunctionFlags` for `FUNC_Native` and `FUNC_HasOutParms`, found by
//!   reading the machine code of each virtual function. Nothing is executed.

use std::collections::BTreeSet;

use ufathom_model::flags::{FUNC_HAS_OUT_PARMS, FUNC_NATIVE, RF_CLASS_DEFAULT_OBJECT};

use crate::ctx::{Ctx, le_u32};
use crate::limits::{MAX_VTABLE_SLOTS, PROCESS_EVENT_WINDOW, VTABLE_SAMPLE_OBJECTS};
use crate::result::Globals;
use crate::structs::derives_from;
use crate::universe::Universe;

/// `test dword [reg + disp32], imm32` is `F7 /0`.
const TEST_RM32_IMM32: u8 = 0xF7;

/// `test byte [reg + disp32], imm8` is `F6 /0`: the same test on the byte
/// that holds the flag, which compilers prefer for a single bit.
const TEST_RM8_IMM8: u8 = 0xF6;

/// ModRM `mod` field for a 32-bit displacement.
const MOD_DISP32: u8 = 0b10;

/// ModRM `rm` field that says a SIB byte follows.
const RM_SIB: u8 = 0b100;

/// What the solved layout offers the global search.
pub(crate) struct GlobalsInput<'u> {
    pub universe: &'u Universe,
    pub super_offset: u32,
    pub index_offset: u32,
    pub object_flags_offset: u32,
    pub function_flags_offset: u32,
    pub world: Option<u64>,
    pub game_engine: Option<u64>,
    pub engine: Option<u64>,
}

/// Whether the object at `addr` is a live instance of a class that derives
/// from `base`.
fn is_live_instance(ctx: &Ctx<'_>, input: &GlobalsInput<'_>, addr: u64, base: u64) -> bool {
    let Some(obj) = input.universe.get(addr) else {
        return false;
    };
    let stored_index = ctx.u32(addr + u64::from(input.index_offset));
    let flags = ctx.u32(addr + u64::from(input.object_flags_offset));
    stored_index == Some(obj.index)
        && flags.is_some_and(|f| f & RF_CLASS_DEFAULT_OBJECT == 0)
        && derives_from(ctx, input.super_offset, obj.class, base)
}

fn slots_holding(
    ctx: &Ctx<'_>,
    input: &GlobalsInput<'_>,
    slots: &[(u64, u64)],
    base: u64,
) -> Vec<u64> {
    let mut out: Vec<u64> = slots
        .iter()
        .copied()
        .filter(|&(_, pointer)| is_live_instance(ctx, input, pointer, base))
        .map(|(slot, _)| ctx.rva(slot))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether `code` holds a test of `flags_offset` against `mask`, in either of
/// the encodings compilers emit.
fn tests_flag(code: &[u8], flags_offset: u32, mask: u32) -> bool {
    let (byte_offset, byte_mask) = match mask {
        FUNC_NATIVE => (flags_offset + 1, 0x04u8),
        FUNC_HAS_OUT_PARMS => (flags_offset + 2, 0x40u8),
        _ => (u32::MAX, 0),
    };
    code.iter().enumerate().any(|(i, &opcode)| {
        let wide = opcode == TEST_RM32_IMM32;
        let narrow = opcode == TEST_RM8_IMM8;
        if !wide && !narrow {
            return false;
        }
        let Some(&modrm) = code.get(i + 1) else {
            return false;
        };
        if modrm >> 6 != MOD_DISP32 || (modrm >> 3) & 7 != 0 {
            return false;
        }
        let sib = usize::from(modrm & 7 == RM_SIB);
        let disp_at = i + 2 + sib;
        let Some(disp) = le_u32(code, disp_at) else {
            return false;
        };
        if wide {
            disp == flags_offset && le_u32(code, disp_at + 4) == Some(mask)
        } else {
            disp == byte_offset && code.get(disp_at + 4) == Some(&byte_mask)
        }
    })
}

/// Function pointers of the vtables of a sample of objects, as a set of
/// known function starts.
fn known_function_starts(ctx: &Ctx<'_>, universe: &Universe) -> BTreeSet<u64> {
    let mut vtables: BTreeSet<u64> = BTreeSet::new();
    for obj in universe
        .objs
        .iter()
        .step_by((universe.objs.len() / 512).max(1))
    {
        if let Some(vtable) = ctx.u64(obj.addr)
            && ctx.in_image(vtable)
        {
            vtables.insert(vtable);
        }
        if vtables.len() >= VTABLE_SAMPLE_OBJECTS {
            break;
        }
    }
    let mut starts = BTreeSet::new();
    for vtable in vtables {
        for slot in vtable_slots(ctx, vtable) {
            starts.insert(slot);
        }
    }
    starts
}

/// The function pointers of a vtable: consecutive slots that point into an
/// image.
fn vtable_slots(ctx: &Ctx<'_>, vtable: u64) -> Vec<u64> {
    let Some(bytes) = ctx.read(vtable, MAX_VTABLE_SLOTS * 8).or_else(|| {
        let available = usize::try_from(ctx.readable_extent(vtable)).ok()?;
        ctx.read(vtable, (available / 8 * 8).min(MAX_VTABLE_SLOTS * 8))
    }) else {
        return Vec::new();
    };
    bytes
        .chunks_exact(8)
        .map_while(|chunk| {
            let value = u64::from_le_bytes(<[u8; 8]>::try_from(chunk).ok()?);
            ctx.in_image(value).then_some(value)
        })
        .collect()
}

/// Finds `ProcessEvent` in the vtable of the object in slot 0.
fn find_process_event(ctx: &Ctx<'_>, input: &GlobalsInput<'_>) -> (Option<u32>, Option<u64>) {
    let Some(first) = input.universe.objs.first() else {
        return (None, None);
    };
    let Some(vtable) = ctx.u64(first.addr).filter(|&v| ctx.in_image(v)) else {
        ctx.warn("process_event: object 0 has no vtable in an image");
        return (None, None);
    };
    let slots = vtable_slots(ctx, vtable);
    let starts = known_function_starts(ctx, input.universe);
    let mut found: Vec<(u32, u64)> = Vec::new();
    for (index, &start) in slots.iter().enumerate() {
        let end = starts
            .range(start + 1..)
            .next()
            .copied()
            .unwrap_or(u64::MAX)
            .min(start.saturating_add(PROCESS_EVENT_WINDOW as u64));
        let len = usize::try_from(end - start).unwrap_or(PROCESS_EVENT_WINDOW);
        let code = ctx.read_prefix(start, len);
        if tests_flag(&code, input.function_flags_offset, FUNC_NATIVE)
            && tests_flag(&code, input.function_flags_offset, FUNC_HAS_OUT_PARMS)
        {
            found.push((u32::try_from(index).unwrap_or(u32::MAX), start));
        }
    }
    // The slot list may run on into the next table, which repeats inherited
    // functions: one function is one candidate, at its lowest index.
    found.sort_by_key(|&(index, start)| (start, index));
    found.dedup_by_key(|&mut (_, start)| start);
    found.sort_by_key(|&(index, _)| index);
    match found.as_slice() {
        [(index, start)] => {
            ctx.evidence(
                "globals.process_event_index",
                u64::from(*index),
                "the virtual function of object 0 that tests FunctionFlags for FUNC_Native and FUNC_HasOutParms",
                slots.len(),
                1,
            );
            (Some(*index), Some(ctx.rva(*start)))
        }
        [] => {
            ctx.warn(format!(
                "process_event: none of the {} virtual functions of object 0 tests FunctionFlags for FUNC_Native and FUNC_HasOutParms",
                slots.len()
            ));
            (None, None)
        }
        many => {
            ctx.warn(format!(
                "process_event: {} virtual functions of object 0 test both flags; none is chosen",
                many.len()
            ));
            (None, None)
        }
    }
}

/// Finds the globals.
pub(crate) fn find_globals(ctx: &Ctx<'_>, input: &GlobalsInput<'_>) -> Globals {
    let slots = ctx.pointer_slots(false);
    let gworld = input
        .world
        .map(|world| slots_holding(ctx, input, &slots, world))
        .unwrap_or_default();
    let engine_base = input.game_engine.or(input.engine);
    let gengine = engine_base
        .map(|base| slots_holding(ctx, input, &slots, base))
        .unwrap_or_default();
    if input.game_engine.is_none() && input.engine.is_some() {
        ctx.warn("gengine: no GameEngine class in the image; slots holding any Engine instance are listed");
    }
    for (name, slots) in [("gworld", &gworld), ("gengine", &gengine)] {
        match slots.len() {
            0 => ctx.warn(format!("{name}: no slot holds a live instance")),
            1 => {}
            n => ctx.warn(format!(
                "{name}: {n} slots hold a live instance; none is chosen"
            )),
        }
    }
    let (process_event_index, process_event_rva) = find_process_event(ctx, input);
    Globals {
        module: ctx.module_name.clone(),
        gworld,
        gengine,
        process_event_index,
        process_event_rva,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "test code may panic on failure"
    )]

    use super::*;

    #[test]
    fn recognises_both_encodings_of_the_flag_tests() {
        let flags = 0xB8u32;
        let mut wide = vec![0x48, 0xF7, 0x82];
        wide.extend(flags.to_le_bytes());
        wide.extend(FUNC_NATIVE.to_le_bytes());
        assert!(tests_flag(&wide, flags, FUNC_NATIVE));
        assert!(!tests_flag(&wide, flags, FUNC_HAS_OUT_PARMS));
        assert!(!tests_flag(&wide, flags + 4, FUNC_NATIVE));

        let mut sib = vec![0xF7, 0x84, 0x24];
        sib.extend(flags.to_le_bytes());
        sib.extend(FUNC_HAS_OUT_PARMS.to_le_bytes());
        assert!(tests_flag(&sib, flags, FUNC_HAS_OUT_PARMS));

        let mut narrow = vec![0xF6, 0x83];
        narrow.extend((flags + 2).to_le_bytes());
        narrow.push(0x40);
        assert!(tests_flag(&narrow, flags, FUNC_HAS_OUT_PARMS));
        assert!(!tests_flag(&narrow, flags, FUNC_NATIVE));
    }
}
