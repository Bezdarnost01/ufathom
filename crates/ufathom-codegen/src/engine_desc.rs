//! The `ENGINE` description and the session helpers written into `lib.rs`.

use std::fmt::Write as _;

use ufathom_model::Dump;

use crate::build::Built;
use ufathom_model::layout::{Location, NameTableLayout, ObjectArrayKind, TransformOp};

/// Items appended to `lib.rs`: `ENGINE`, `session` and, behind the `inproc`
/// feature, `attach`.
pub(crate) fn lib_items(dump: &Dump, built: &Built) -> String {
    let l = &dump.layout;
    let rt = "::ufathom_runtime";
    let names = match &l.names {
        NameTableLayout::Pool(p) => format!(
            "{rt}::NamesDesc::Pool({rt}::NamePoolDesc {{
        location: {},
        blocks: {:#x},
        stride: {},
        block_offset_bits: {},
        header_offset: {:#x},
        string_offset: {:#x},
        len_shift: {},
        wide_mask: {:#x},
    }})",
            place(&p.location),
            p.blocks,
            p.stride,
            p.block_offset_bits,
            p.entry.header_offset,
            p.entry.string_offset,
            p.entry.len_shift,
            p.entry.wide_mask,
        ),
        NameTableLayout::Array(a) => format!(
            "{rt}::NamesDesc::Array({rt}::NameArrayDesc {{
        location: {},
        indirect: {},
        num_elements: {:#x},
        elements_per_chunk: {},
        index_offset: {:#x},
        string_offset: {:#x},
        wide_mask: {:#x},
    }})",
            place(&a.location),
            a.indirect,
            a.num_elements,
            a.elements_per_chunk,
            a.entry.index_offset,
            a.entry.string_offset,
            a.entry.wide_mask,
        ),
    };
    let o = &l.objects;
    let per_chunk = match o.kind {
        ObjectArrayKind::Fixed => "None".to_owned(),
        ObjectArrayKind::Chunked { elements_per_chunk } => format!("Some({elements_per_chunk})"),
    };
    let transform: Vec<String> = o.pointer_transform.iter().map(transform).collect();
    let g = &dump.globals;

    let mut out = String::new();
    let _ = write!(
        out,
        "
/// The engine build this SDK was generated from: where the name table and the
/// object array live and the offsets a [`{rt}::Session`] needs.
pub static ENGINE: {rt}::EngineDesc = {rt}::EngineDesc {{
    names: {names},
    objects: {rt}::ObjectArrayDesc {{
        location: {},
        elements_per_chunk: {per_chunk},
        objects: {:#x},
        num_elements: {:#x},
        item_size: {:#x},
        item_object: {:#x},
        item_serial: {},
        pointer_transform: &[{}],
    }},
    fname: {rt}::FNameDesc {{
        size: {},
        comparison_index: {:#x},
        display_index: {},
        number: {},
    }},
    object: {rt}::ObjectDesc {{
        flags: {:#x},
        index: {:#x},
        class: {:#x},
        name: {:#x},
        outer: {:#x},
        super_struct: {:#x},
        properties_size: {:#x},
        default_object: {},
    }},
    process_event_index: {},
    gworld: {},
    gengine: {},
}};

/// Opens a session over `memory` for this build: names, object paths,
/// lookups, `IsA`, class default objects, `GWorld` and `GEngine`.
///
/// Inside the game pass `{rt}::mem::SelfMemory::new()`; from another process
/// pass a `{rt}::mem::ProcessMemory`.
///
/// # Errors
///
/// As [`{rt}::Session::new`].
pub fn session<M: {rt}::MemoryReader>(
    memory: M,
) -> Result<{rt}::Session<M>, {rt}::SessionError> {{
    {rt}::Session::new(memory, &ENGINE)
}}

/// Attaches an executor that calls game functions through `ProcessEvent`.
/// Its [`session`]({rt}::inproc::InProcessExecutor::session) gives the
/// lookups as well.
///
/// # Safety
///
/// As [`{rt}::inproc::InProcessExecutor::attach`]: this code runs inside the
/// game this SDK was generated from, and every call is made on the game
/// thread.
///
/// # Errors
///
/// As [`{rt}::inproc::InProcessExecutor::attach`].
#[cfg(feature = \"inproc\")]
pub unsafe fn attach() -> Result<{rt}::inproc::InProcessExecutor, {rt}::SessionError> {{
    // SAFETY: the caller upholds the contract above, which is the contract of
    // `InProcessExecutor::attach` for `ENGINE`.
    unsafe {{ {rt}::inproc::InProcessExecutor::attach(&ENGINE) }}
}}

/// Hooks `ProcessEvent` to run code on the game thread: `run` and `call`
/// queue tasks that get an executor, `before` and `after` intercept calls of
/// one game function. Installs once; later calls return the same hook.
///
/// # Safety
///
/// As [`{rt}::inproc::GameThread::install`]: this code runs inside the game
/// this SDK was generated from, and this module stays loaded while the hook
/// is installed.
///
/// # Errors
///
/// As [`{rt}::inproc::GameThread::install`].
#[cfg(feature = \"inproc\")]
pub unsafe fn game_thread() -> Result<&'static {rt}::inproc::GameThread, {rt}::inproc::HookError> {{
    // SAFETY: the caller upholds the contract above, which is the contract of
    // `GameThread::install` for `ENGINE`.
    unsafe {{ {rt}::inproc::GameThread::install(&ENGINE) }}
}}
",
        place(&o.location),
        o.objects,
        o.num_elements,
        o.item_size,
        o.item_object,
        opt_hex(o.item_serial),
        transform.join(", "),
        l.fname.size,
        l.fname.comparison_index,
        opt_hex(l.fname.display_index),
        opt_hex(l.fname.number),
        l.uobject.flags,
        l.uobject.index,
        l.uobject.class,
        l.uobject.name,
        l.uobject.outer,
        l.ustruct.super_struct,
        l.ustruct.properties_size,
        opt_hex(l.uclass.default_object),
        g.process_event_index
            .map_or_else(|| "None".to_owned(), |i| format!("Some({i})")),
        opt_place(g.gworld.as_ref()),
        opt_place(g.gengine.as_ref()),
    );
    for (name, path, what) in [
        ("gworld", "/Script/Engine.World", "`GWorld`, the live world"),
        (
            "gengine",
            "/Script/Engine.Engine",
            "`GEngine`, the live engine",
        ),
    ] {
        let Some(class) = built.structs.iter().find(|s| s.path == path) else {
            continue;
        };
        let _ = write!(
            out,
            "
/// {what}, as the generated `{ty}`. `None` while there is none.
///
/// # Errors
///
/// As [`{rt}::Session::{name}`].
#[cfg(feature = \"{module}\")]
pub fn {name}<M: {rt}::MemoryReader>(
    session: &{rt}::Session<M>,
) -> Result<Option<{rt}::Ptr<{module}::{ty}>>, {rt}::SessionError> {{
    Ok(session.{name}()?.map(|object| object.cast()))
}}
",
            ty = class.rust_name,
            module = class.module,
        );
    }
    out
}

fn place(location: &Location) -> String {
    match location {
        Location::Rva { module, rva } => {
            format!("::ufathom_runtime::Place::Rva {{ module: {module:?}, rva: {rva:#x} }}")
        }
        Location::Absolute(addr) => format!("::ufathom_runtime::Place::Absolute({addr:#x})"),
    }
}

fn opt_place(location: Option<&Location>) -> String {
    location.map_or_else(|| "None".to_owned(), |l| format!("Some({})", place(l)))
}

fn opt_hex(value: Option<u32>) -> String {
    value.map_or_else(|| "None".to_owned(), |v| format!("Some({v:#x})"))
}

fn transform(op: &TransformOp) -> String {
    let rt = "::ufathom_runtime::Transform";
    match op {
        TransformOp::Xor(v) => format!("{rt}::Xor({v:#x})"),
        TransformOp::Add(v) => format!("{rt}::Add({v:#x})"),
        TransformOp::Sub(v) => format!("{rt}::Sub({v:#x})"),
        TransformOp::RotateLeft(n) => format!("{rt}::RotateLeft({n})"),
        TransformOp::RotateRight(n) => format!("{rt}::RotateRight({n})"),
        TransformOp::Not => format!("{rt}::Not"),
    }
}
