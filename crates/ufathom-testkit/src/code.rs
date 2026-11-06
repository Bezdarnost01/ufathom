//! Fake machine code and vtables.
//!
//! The functions are byte sequences assembled from a palette of ordinary
//! x64 instruction encodings. They are never executed; they exist so that a
//! scan of `.text` sees realistic bytes. Exactly one function, the
//! `UObject::ProcessEvent` candidate, contains both
//! `test dword [reg+FunctionFlags], FUNC_Native` and
//! `test dword [reg+FunctionFlags], FUNC_HasOutParms`.

use std::collections::BTreeMap;

use ufathom_model::flags::{FUNC_FINAL, FUNC_HAS_OUT_PARMS, FUNC_NATIVE};

use crate::profile::Profile;
use crate::rng::Rng;
use crate::space::Mem;

/// `test dword [reg + disp32], imm32` as `F7 /0`: the opcode byte.
const TEST_RM32_IMM32: u8 = 0xF7;
/// ModRM byte for `[rdx + disp32]` with `/0`: mod = 10, reg = 000, rm = 010.
const MODRM_RDX_DISP32: u8 = 0x82;
/// ModRM byte for `[rbx + disp32]`.
const MODRM_RBX_DISP32: u8 = 0x83;

/// Encodes `test dword [reg + disp32], imm32`.
fn test_flags(modrm: u8, disp: u32, imm: u32) -> Vec<u8> {
    let mut bytes = vec![TEST_RM32_IMM32, modrm];
    bytes.extend_from_slice(&disp.to_le_bytes());
    bytes.extend_from_slice(&imm.to_le_bytes());
    bytes
}

/// One instruction picked from the palette. None of them contains the `F7`
/// opcode byte, so they cannot form a `test [reg+disp32], imm32` by accident.
fn palette(rng: &mut Rng, here: u64, targets: &[u64]) -> Vec<u8> {
    let small = u8::try_from(rng.range(8, 0x78)).unwrap_or(8) & !3;
    match rng.below(14) {
        0 => vec![0x48, 0x89, 0x5C, 0x24, (small & 0x78) | 8],
        1 => vec![0x48, 0x8B, 0xD9],
        2 => vec![0x33, 0xC0],
        3 => vec![0x8B, 0x41, small],
        4 => vec![0x48, 0x8B, 0x01],
        5 => vec![0xFF, 0x50, small],
        6 => vec![0x85, 0xC0],
        7 => vec![0x74, u8::try_from(rng.range(2, 12)).unwrap_or(2)],
        8 => vec![0x0F, 0xB6, 0x41, small],
        9 => vec![0x4C, 0x8B, 0xC1],
        10 => vec![0x89, 0x41, small],
        11 => {
            let mut bytes = vec![0x41, 0xB8];
            // Avoid the opcode byte inside the immediate for a clean scan.
            bytes.extend(
                rng.next_u32()
                    .to_le_bytes()
                    .map(|b| if b == TEST_RM32_IMM32 { 0x11 } else { b }),
            );
            bytes
        }
        12 => {
            // call rel32 to an already emitted function, or a nearby address.
            let target = rng.pick(targets).copied().unwrap_or(here);
            let rel = target.wrapping_sub(here + 5);
            let mut bytes = vec![0xE8];
            let rel32 = u32::try_from(rel & 0xFFFF_FFFF).unwrap_or(0);
            bytes.extend(
                rel32
                    .to_le_bytes()
                    .map(|b| if b == TEST_RM32_IMM32 { 0x12 } else { b }),
            );
            bytes
        }
        _ => vec![0x48, 0x85, 0xC0],
    }
}

const PROLOGUES: [&[u8]; 4] = [
    &[0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20],
    &[0x48, 0x83, 0xEC, 0x28],
    &[0x40, 0x53, 0x48, 0x83, 0xEC, 0x20],
    &[0x48, 0x89, 0x74, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x30],
];

/// Epilogue matching the second prologue family; the exact stack adjustment is
/// irrelevant because the code never runs.
const EPILOGUE: [u8; 5] = [0x48, 0x83, 0xC4, 0x28, 0xC3];

/// An ordinary function body of 16 to 100 bytes.
fn ordinary_function(rng: &mut Rng, here: u64, targets: &[u64]) -> Vec<u8> {
    let mut body: Vec<u8> = rng.pick(&PROLOGUES).copied().unwrap_or(&[0xC3]).to_vec();
    let instructions = rng.range(3, 14);
    for _ in 0..instructions {
        let at = here + body.len() as u64;
        body.extend(palette(rng, at, targets));
    }
    body.extend_from_slice(&EPILOGUE);
    body
}

/// Code and vtable state of an image under construction.
#[derive(Debug)]
pub struct Code {
    rng: Rng,
    /// Address of the `ProcessEvent` candidate.
    pub process_event: u64,
    /// Addresses of functions that contain only part of the
    /// `ProcessEvent` signature, or the right bytes at the wrong displacement.
    pub decoys: Vec<u64>,
    /// Address of the entry point.
    pub entry: u64,
    pe_index: usize,
    slot_count: usize,
    rtti: u64,
    known: Vec<u64>,
    slots: BTreeMap<String, Vec<u64>>,
    tables: BTreeMap<String, u64>,
}

impl Code {
    /// Emits the `ProcessEvent` candidate, its decoys and the RTTI stand-in.
    pub fn new(mem: &mut Mem, profile: &Profile, rng: Rng) -> Self {
        let mut code = Self {
            rng,
            process_event: 0,
            decoys: Vec::new(),
            entry: 0,
            pe_index: usize::try_from(profile.shape.process_event_index).unwrap_or(0x40),
            slot_count: usize::try_from(profile.shape.vtable_slots).unwrap_or(0x60),
            rtti: 0,
            known: Vec::new(),
            slots: BTreeMap::new(),
            tables: BTreeMap::new(),
        };
        let flags = profile.layout.ufunction.flags;

        code.entry = code.ordinary(mem);
        code.process_event = code.process_event_function(mem, flags);
        // Right displacement, only one of the two tests.
        let native_only = code.function_with(mem, &[(MODRM_RBX_DISP32, flags, FUNC_NATIVE)]);
        let outparms_only =
            code.function_with(mem, &[(MODRM_RBX_DISP32, flags, FUNC_HAS_OUT_PARMS)]);
        // Both tests, but at the wrong displacement.
        let wrong_disp = code.function_with(
            mem,
            &[
                (MODRM_RDX_DISP32, flags + 0x10, FUNC_NATIVE),
                (MODRM_RDX_DISP32, flags + 0x10, FUNC_HAS_OUT_PARMS),
            ],
        );
        // The right displacement with an unrelated flag.
        let other_flag = code.function_with(mem, &[(MODRM_RDX_DISP32, flags, FUNC_FINAL)]);
        code.decoys = vec![native_only, outparms_only, wrong_disp, other_flag];

        // A stand-in for the RTTI complete object locator every MSVC vtable
        // points to from slot -1.
        code.rtti = mem.rdata.alloc(24, 8);
        code
    }

    /// Emits an ordinary function and returns its address.
    pub fn ordinary(&mut self, mem: &mut Mem) -> u64 {
        let here = mem.text.next_address(16);
        let bytes = ordinary_function(&mut self.rng, here, &self.known);
        let addr = mem.emit_text(&bytes, 16);
        self.known.push(addr);
        addr
    }

    /// Emits a function whose body contains the given
    /// `(modrm, displacement, flag)` tests between ordinary instructions.
    fn function_with(&mut self, mem: &mut Mem, tests: &[(u8, u32, u32)]) -> u64 {
        let here = mem.text.next_address(16);
        let mut bytes = vec![0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x83, 0xEC, 0x20];
        for &(modrm, disp, flag) in tests {
            for _ in 0..self.rng.range(1, 4) {
                let at = here + bytes.len() as u64;
                bytes.extend(palette(&mut self.rng, at, &self.known));
            }
            bytes.extend(test_flags(modrm, disp, flag));
            // jz rel8 over a short block
            bytes.extend_from_slice(&[0x74, 0x04, 0x33, 0xC0, 0xEB, 0x02]);
        }
        bytes.extend_from_slice(&EPILOGUE);
        let addr = mem.emit_text(&bytes, 16);
        self.known.push(addr);
        addr
    }

    fn process_event_function(&mut self, mem: &mut Mem, flags: u32) -> u64 {
        // ProcessEvent receives the Function in rdx. It tests FUNC_Native
        // early (to choose between a native call and the script VM) and
        // FUNC_HasOutParms later (to copy out parameters).
        self.function_with(
            mem,
            &[
                (MODRM_RDX_DISP32, flags, FUNC_NATIVE),
                (MODRM_RDX_DISP32, flags, FUNC_HAS_OUT_PARMS),
            ],
        )
    }

    /// Emits a native exec thunk, the target of `UFunction::Func`.
    pub fn native_thunk(&mut self, mem: &mut Mem) -> u64 {
        self.ordinary(mem)
    }

    fn fresh_slots(&mut self, mem: &mut Mem) -> Vec<u64> {
        let mut slots = Vec::with_capacity(self.slot_count);
        for index in 0..self.slot_count {
            let slot = match index {
                i if i == self.pe_index => self.process_event,
                // Neighbours of ProcessEvent are the single-test decoys.
                i if i + 1 == self.pe_index => self.decoys.first().copied().unwrap_or(0),
                i if i == self.pe_index + 1 => self.decoys.get(1).copied().unwrap_or(0),
                i if i == self.pe_index + 3 => self.decoys.get(2).copied().unwrap_or(0),
                i if i == self.pe_index + 4 => self.decoys.get(3).copied().unwrap_or(0),
                _ => self.ordinary(mem),
            };
            slots.push(slot);
        }
        slots
    }

    /// The vtable of C++ class `key`, created on first use. A class with a
    /// `parent` starts from the parent's slots and overrides a few of them,
    /// like a derived class does; `ProcessEvent` is never overridden.
    pub fn vtable(&mut self, mem: &mut Mem, key: &str, parent: Option<&str>) -> u64 {
        if let Some(&addr) = self.tables.get(key) {
            return addr;
        }
        let mut slots = match parent {
            Some(parent_key) => {
                let _ = self.vtable(mem, parent_key, None);
                self.slots.get(parent_key).cloned().unwrap_or_default()
            }
            None => Vec::new(),
        };
        if slots.is_empty() {
            slots = self.fresh_slots(mem);
        } else {
            for _ in 0..self.rng.range(2, 7) {
                let index = usize::try_from(self.rng.below(self.slot_count as u64)).unwrap_or(0);
                let protected = index + 1 >= self.pe_index && index <= self.pe_index + 4;
                if !protected {
                    let replacement = self.ordinary(mem);
                    if let Some(slot) = slots.get_mut(index) {
                        *slot = replacement;
                    }
                }
            }
        }
        let table = mem.rdata.alloc(8 + slots.len() * 8, 16);
        mem.put_u64(table, self.rtti);
        for (i, &slot) in slots.iter().enumerate() {
            mem.put_u64(table + 8 + i as u64 * 8, slot);
        }
        let addr = table + 8;
        self.slots.insert(key.to_owned(), slots);
        self.tables.insert(key.to_owned(), addr);
        addr
    }

    /// Emits `count` extra functions that nothing refers to by vtable, and a
    /// pointer table to some of them in `.rdata`, so function pointers are
    /// not found only inside vtables.
    pub fn noise(&mut self, mem: &mut Mem, count: usize) {
        for _ in 0..count {
            let _ = self.ordinary(mem);
        }
        let entries = self.known.len().min(48);
        let table = mem.rdata.alloc(entries * 8, 16);
        for i in 0..entries {
            let pick = self.rng.pick(&self.known).copied().unwrap_or(0);
            mem.put_u64(table + i as u64 * 8, pick);
        }
    }

    /// All function addresses emitted so far.
    #[must_use]
    pub fn functions(&self) -> &[u64] {
        &self.known
    }
}
