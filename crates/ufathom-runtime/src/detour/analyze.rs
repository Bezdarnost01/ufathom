//! Decoding the start of a function: which whole instructions a 5-byte jump
//! overwrites, whether they can be moved, and how to move them.
//!
//! Decoding and re-encoding are done by `iced-x86`; nothing here measures an
//! instruction length by hand.

use iced_x86::{
    BlockEncoder, BlockEncoderOptions, Decoder, DecoderOptions, FlowControl, Instruction,
    InstructionBlock, OpKind,
};

use super::DetourError;

/// Size of the `jmp rel32` written over the start of the function.
pub(super) const JUMP_LEN: usize = 5;

/// How many bytes are read from the target to decode the stolen
/// instructions: the last instruction can start at byte 4 and be 15 bytes
/// long, so 19 would do; the rest is slack.
pub(super) const WINDOW_LEN: usize = 32;

/// The instructions a detour overwrites.
pub(super) struct Prologue {
    /// Total length of the instructions, at least [`JUMP_LEN`].
    pub len: usize,
    /// The instructions, with their original addresses.
    pub instructions: Vec<Instruction>,
    /// Offset of each instruction from the target.
    pub offsets: Vec<u32>,
    /// Absolute addresses the moved code refers to with a 32-bit
    /// displacement: relative branches and RIP-relative operands.
    pub anchors: Vec<usize>,
}

/// The prologue moved to another address.
pub(super) struct Relocated {
    /// The machine code.
    pub code: Vec<u8>,
    /// Offset of each moved instruction from the start of `code`.
    pub offsets: Vec<u32>,
}

fn refuse(address: usize, reason: impl Into<String>) -> DetourError {
    DetourError::Unrelocatable {
        address,
        reason: reason.into(),
    }
}

/// Decodes whole instructions from `bytes` (the code at `address`) until at
/// least [`JUMP_LEN`] bytes are covered, and checks they can be moved.
///
/// Refused: `ret`, `int3` and other interrupts and traps, a jump with more
/// stolen bytes behind it (the code after it is unreachable, so the
/// function's real layout is not what it looks like), a jump into the stolen
/// bytes other than the next instruction, far branches, and bytes that do not
/// decode.
///
/// A jump that is the last stolen instruction is allowed: it is how
/// import thunks start.
pub(super) fn analyze(address: usize, bytes: &[u8]) -> Result<Prologue, DetourError> {
    let mut decoder = Decoder::with_ip(64, bytes, address as u64, DecoderOptions::NONE);
    let mut instructions = Vec::new();
    let mut offsets = Vec::new();
    let mut consumed = 0usize;
    while consumed < JUMP_LEN {
        if !decoder.can_decode() {
            return Err(refuse(
                address,
                "the readable code ends before 5 bytes of whole instructions",
            ));
        }
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return Err(refuse(
                address,
                format!("the bytes at offset {consumed} are not a valid instruction"),
            ));
        }
        offsets.push(u32::try_from(consumed).map_err(|_| refuse(address, "offset overflow"))?);
        consumed += instruction.len();
        let last = consumed >= JUMP_LEN;
        check_flow(address, consumed - instruction.len(), &instruction, last)?;
        instructions.push(instruction);
    }

    let start = address as u64;
    let end = start.saturating_add(consumed as u64);
    let mut anchors = Vec::new();
    for instruction in &instructions {
        if let Some(target) = branch_target(instruction) {
            let inside = target >= start && target < end;
            if inside && target != instruction.next_ip() {
                return Err(refuse(
                    address,
                    format!(
                        "a branch at offset {} jumps into the bytes to overwrite (to offset {})",
                        instruction.ip().saturating_sub(start),
                        target - start
                    ),
                ));
            }
            if !inside {
                anchors.push(to_address(address, target)?);
            }
        }
        if instruction.is_ip_rel_memory_operand() {
            anchors.push(to_address(address, instruction.ip_rel_memory_address())?);
        }
    }
    Ok(Prologue {
        len: consumed,
        instructions,
        offsets,
        anchors,
    })
}

fn to_address(address: usize, value: u64) -> Result<usize, DetourError> {
    usize::try_from(value).map_err(|_| refuse(address, "an operand address does not fit"))
}

/// The target of a relative near branch, when the instruction is one.
fn branch_target(instruction: &Instruction) -> Option<u64> {
    (instruction.op_count() > 0 && instruction.op0_kind() == OpKind::NearBranch64)
        .then(|| instruction.near_branch_target())
}

fn check_flow(
    address: usize,
    offset: usize,
    instruction: &Instruction,
    last: bool,
) -> Result<(), DetourError> {
    for operand in 0..instruction.op_count() {
        if matches!(
            instruction.op_kind(operand),
            OpKind::FarBranch16 | OpKind::FarBranch32
        ) {
            return Err(refuse(address, format!("a far branch at offset {offset}")));
        }
    }
    match instruction.flow_control() {
        FlowControl::Return => Err(refuse(
            address,
            format!("a ret at offset {offset}: the function is too short to detour"),
        )),
        FlowControl::Interrupt | FlowControl::Exception => Err(refuse(
            address,
            format!("an interrupt or trap (int3, ud2, ...) at offset {offset}"),
        )),
        FlowControl::XbeginXabortXend => Err(refuse(
            address,
            format!("a transactional instruction at offset {offset}"),
        )),
        FlowControl::UnconditionalBranch | FlowControl::IndirectBranch if !last => Err(refuse(
            address,
            format!(
                "a jump at offset {offset} that ends the function with bytes still to overwrite"
            ),
        )),
        _ => Ok(()),
    }
}

/// Re-encodes the prologue so that it runs at `at` and behaves as it did at
/// the original address: relative branches and RIP-relative operands are
/// adjusted, and a branch to the next stolen instruction stays inside the
/// moved code.
///
/// # Errors
///
/// [`DetourError::Unrelocatable`] when an operand does not fit a 32-bit
/// displacement from `at`.
pub(super) fn relocate(
    address: usize,
    prologue: &Prologue,
    at: usize,
) -> Result<Relocated, DetourError> {
    let block = InstructionBlock::new(&prologue.instructions, at as u64);
    let result = BlockEncoder::encode(
        64,
        block,
        BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
    )
    .map_err(|error| refuse(address, format!("cannot move the instructions: {error}")))?;
    if result.new_instruction_offsets.len() != prologue.instructions.len() {
        return Err(refuse(address, "the encoder lost track of an instruction"));
    }
    Ok(Relocated {
        code: result.code_buffer,
        offsets: result.new_instruction_offsets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: usize = 0x1_4000_0000;

    #[test]
    fn covers_whole_instructions() {
        // push rbx; sub rsp, 0x20; mov eax, ecx
        let bytes = [0x53, 0x48, 0x83, 0xEC, 0x20, 0x89, 0xC8, 0x90];
        let prologue = analyze(ADDRESS, &bytes).unwrap();
        assert_eq!(prologue.len, 5);
        assert_eq!(prologue.offsets, [0, 1]);
        // `mov rax, imm64` is 10 bytes: 3 + 10 are stolen.
        let bytes = [0x90, 0x90, 0x90, 0x48, 0xB8, 1, 2, 3, 4, 5, 6, 7, 8, 0x90];
        assert_eq!(analyze(ADDRESS, &bytes).unwrap().len, 13);
    }

    #[test]
    fn refuses_what_cannot_move() {
        let ret = [0xB0, 0x01, 0xC3, 0x90, 0x90, 0x90];
        let int3 = [0x90, 0xCC, 0x90, 0x90, 0x90, 0x90];
        // test ecx, ecx; jz back to the start
        let back = [0x85, 0xC9, 0x74, 0xFC, 0x90, 0x90];
        // jmp +0; then three more bytes
        let jump_then_more = [0xEB, 0x00, 0x90, 0x90, 0x90, 0x90];
        let garbage = [0x06, 0x06, 0x06, 0x06, 0x06, 0x06];
        for bytes in [&ret[..], &int3, &back, &jump_then_more, &garbage] {
            assert!(
                matches!(
                    analyze(ADDRESS, bytes),
                    Err(DetourError::Unrelocatable { .. })
                ),
                "{bytes:02x?}"
            );
        }
        // Too few bytes to cover 5.
        assert!(analyze(ADDRESS, &[0x90, 0x90]).is_err());
    }

    #[test]
    fn a_jump_that_ends_the_stolen_bytes_is_allowed() {
        // An import thunk: jmp qword ptr [rip + 0x1000] (6 bytes).
        let bytes = [0xFF, 0x25, 0x00, 0x10, 0x00, 0x00, 0xCC];
        let prologue = analyze(ADDRESS, &bytes).unwrap();
        assert_eq!(prologue.len, 6);
        assert_eq!(prologue.anchors, [ADDRESS + 6 + 0x1000]);
    }

    #[test]
    fn rip_relative_code_gets_a_new_displacement() {
        // lea rax, [rip + 0x100]
        let bytes = [0x48, 0x8D, 0x05, 0x00, 0x01, 0x00, 0x00, 0x90, 0x90];
        let prologue = analyze(ADDRESS, &bytes).unwrap();
        let moved = relocate(ADDRESS, &prologue, ADDRESS + 0x1000).unwrap();
        // The moved code is 0x1000 farther on, so the displacement shrinks
        // from 0x100 to 0x100 - 0x1000 = -0xF00.
        assert_eq!(moved.code, [0x48, 0x8D, 0x05, 0x00, 0xF1, 0xFF, 0xFF]);
        assert_eq!(moved.offsets, [0]);
    }
}
