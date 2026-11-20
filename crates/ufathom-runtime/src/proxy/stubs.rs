//! The assembly: one tiny stub per export, and one resolver per preset.
//!
//! Every stub, for ordinal `N` of preset `P`, is
//!
//! ```text
//! ufathom_proxy_P_N:
//!     mov  r11, [slots_P + 8*N]      ; the cached address of the real function
//!     test r11, r11
//!     jz   cold                      ; first call: ask the resolver
//!     jmp  r11                       ; tail call, arguments untouched
//! cold:
//!     mov  r11d, N
//!     jmp  ufathom_proxy_P_resolver
//! ```
//!
//! and the resolver saves the argument registers, calls
//! [`resolve_entry`](super::resolve::resolve_entry), restores them and jumps to
//! the address it returned. The slots are ordinary Rust atomics, so the Rust
//! side writes an address once and every later call is a load and a jump.
//!
//! `r11` is volatile and carries no argument in the Windows x64 calling
//! convention, so a stub may overwrite it. `rax` is not an argument register
//! either; it only holds the return value.

use core::sync::atomic::AtomicUsize;

use super::resolve::{Forwarder, resolve_entry};

/// Bytes the resolver reserves below the return address: 32 of shadow space
/// for the Rust callee, 32 for the integer argument registers, 64 for
/// `xmm0`-`xmm3`, and 8 so that the stack is 16-byte aligned at the `call`
/// (the stack is 8 bytes off when a function is entered).
const RESOLVER_FRAME: usize = 136;

/// The resolver of the preset whose stubs are `ufathom_proxy_<dll>_<n>`:
/// `$forwarder` is the `static` it passes to `$resolve`.
macro_rules! resolver_asm {
    ($dll:literal, $forwarder:path, $resolve:path) => {
        // Entered by `jmp` from a stub, with the stub's own return address on
        // top of the stack.
        core::arch::global_asm!(
            concat!(
                ".pushsection .text,\"xr\"
",
                ".p2align 4
",
                ".globl ufathom_proxy_", $dll, "_resolver
",
                ".seh_proc ufathom_proxy_", $dll, "_resolver
",
                "ufathom_proxy_", $dll, "_resolver:
",
                "    sub rsp, {frame}
",
                "    .seh_stackalloc {frame}
",
                "    .seh_endprologue
",
                "    mov qword ptr [rsp + 32], rcx
",
                "    mov qword ptr [rsp + 40], rdx
",
                "    mov qword ptr [rsp + 48], r8
",
                "    mov qword ptr [rsp + 56], r9
",
                "    movaps xmmword ptr [rsp + 64], xmm0
",
                "    movaps xmmword ptr [rsp + 80], xmm1
",
                "    movaps xmmword ptr [rsp + 96], xmm2
",
                "    movaps xmmword ptr [rsp + 112], xmm3
",
                "    lea rcx, [rip + {forwarder}]
",
                "    mov rdx, r11
",
                "    call {resolve}
",
                "    mov rcx, qword ptr [rsp + 32]
",
                "    mov rdx, qword ptr [rsp + 40]
",
                "    mov r8, qword ptr [rsp + 48]
",
                "    mov r9, qword ptr [rsp + 56]
",
                "    movaps xmm0, xmmword ptr [rsp + 64]
",
                "    movaps xmm1, xmmword ptr [rsp + 80]
",
                "    movaps xmm2, xmmword ptr [rsp + 96]
",
                "    movaps xmm3, xmmword ptr [rsp + 112]
",
                "    add rsp, {frame}
",
                "    jmp rax
",
                ".seh_endproc
",
                ".popsection
",
            ),
            frame = const RESOLVER_FRAME,
            forwarder = sym $forwarder,
            resolve = sym $resolve,
        );
    };
}

/// The stub for `$ordinal`, reading its address from `$slots`.
macro_rules! stub_asm {
    ($dll:literal, $ordinal:literal, $slots:path) => {
        core::arch::global_asm!(
            concat!(
                ".pushsection .text,\"xr\"
",
                ".p2align 4
",
                ".globl ufathom_proxy_", $dll, "_", $ordinal, "
",
                "ufathom_proxy_", $dll, "_", $ordinal, ":
",
                "    mov r11, qword ptr [rip + {slots} + 8*", $ordinal, "]
",
                "    test r11, r11
",
                "    jz .Lufathom_proxy_", $dll, "_", $ordinal, "_cold
",
                "    jmp r11
",
                ".Lufathom_proxy_", $dll, "_", $ordinal, "_cold:
",
                "    mov r11d, ", $ordinal, "
",
                "    jmp ufathom_proxy_", $dll, "_resolver
",
                ".popsection
",
            ),
            slots = sym $slots,
        );
    };
}

macro_rules! define_stubs {
    ($(
        ($const_name:ident, $id:ident, $dll:literal, $feature:literal, [
            $( ($name:expr, $ordinal:literal, $private:literal) ),* $(,)?
        ])
    ),* $(,)?) => {$(
        #[cfg(feature = $feature)]
        mod $id {
            use super::*;
            use crate::proxy::tables::$const_name;

            const SLOT_COUNT: usize = $const_name.max_ordinal() as usize + 1;

            /// One slot per ordinal; the stubs read them by address.
            static SLOTS: [AtomicUsize; SLOT_COUNT] = [const { AtomicUsize::new(0) }; SLOT_COUNT];

            static FORWARDER: Forwarder = Forwarder::new(&$const_name, &SLOTS);

            resolver_asm!($dll, FORWARDER, resolve_entry);
            $( stub_asm!($dll, $ordinal, SLOTS); )*
        }
    )*};
}

for_each_preset!(define_stubs);

/// The resolver and a stub are tested with a function of their own, so the
/// argument registers and the stack arguments can be checked without any
/// system DLL.
#[cfg(test)]
mod abi_tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::proxy::tables::{Export, Preset};

    const PRESET: Preset = Preset {
        dll: "abitest",
        feature: "none",
        exports: &[Export {
            name: Some("target"),
            ordinal: 1,
            private: false,
        }],
    };
    static SLOTS: [AtomicUsize; 2] = [const { AtomicUsize::new(0) }; 2];
    static FORWARDER: Forwarder = Forwarder::new(&PRESET, &SLOTS);
    static RESOLVED: AtomicUsize = AtomicUsize::new(0);

    /// Stands in for `resolve_entry`: counts the call, wipes every volatile
    /// argument register as Rust code may, and fills the slot.
    extern "C" fn resolve_to_target(_forwarder: *const Forwarder, ordinal: usize) -> usize {
        RESOLVED.fetch_add(1, Ordering::SeqCst);
        // SAFETY: only clobbers registers the asm block declares as outputs.
        unsafe {
            core::arch::asm!(
                "xor ecx, ecx",
                "xor edx, edx",
                "xor r8d, r8d",
                "xor r9d, r9d",
                "pxor xmm0, xmm0",
                "pxor xmm1, xmm1",
                "pxor xmm2, xmm2",
                "pxor xmm3, xmm3",
                out("rcx") _,
                out("rdx") _,
                out("r8") _,
                out("r9") _,
                out("xmm0") _,
                out("xmm1") _,
                out("xmm2") _,
                out("xmm3") _,
            );
        }
        let address = target as *const () as usize;
        if let Some(slot) = SLOTS.get(ordinal) {
            slot.store(address, Ordering::Release);
        }
        address
    }

    resolver_asm!("abitest", FORWARDER, resolve_to_target);
    stub_asm!("abitest", 1, SLOTS);

    unsafe extern "system" {
        fn ufathom_proxy_abitest_1(
            a: i64,
            b: f64,
            c: i64,
            d: f64,
            e: i64,
            f: f64,
            g: i64,
            h: i64,
        ) -> i64;
    }

    /// Integers in `rcx` and `r8`, floats in `xmm1` and `xmm3`, and four
    /// arguments on the stack, one of them a float.
    extern "system" fn target(
        a: i64,
        b: f64,
        c: i64,
        d: f64,
        e: i64,
        f: f64,
        g: i64,
        h: i64,
    ) -> i64 {
        a + (b as i64) * 10
            + c * 100
            + (d as i64) * 1_000
            + e * 10_000
            + (f as i64) * 100_000
            + g * 1_000_000
            + h * 10_000_000
    }

    #[test]
    fn the_resolver_path_keeps_every_argument_register_and_the_stack() {
        const EXPECTED: i64 = 87_654_321;
        // SAFETY: the stub forwards to `target`, which has this signature.
        let call = || unsafe { ufathom_proxy_abitest_1(1, 2.0, 3, 4.0, 5, 6.0, 7, 8) };
        // The first call goes through the resolver, which wipes the registers.
        assert_eq!(call(), EXPECTED);
        assert_eq!(RESOLVED.load(Ordering::SeqCst), 1);
        // The second reads the slot and does not resolve again.
        assert_eq!(call(), EXPECTED);
        assert_eq!(RESOLVED.load(Ordering::SeqCst), 1);
    }
}
