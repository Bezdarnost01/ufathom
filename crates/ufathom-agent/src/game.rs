//! Running code on the game thread, for the opt-in work that calls game
//! functions (see [`preload`](crate::preload)).
//!
//! The dump never calls game code, and this crate never installs anything in
//! the game. The DLL that hosts the session does: it implements
//! [`GameThreadRunner`] with the runtime's `ProcessEvent` hook and hands it to
//! the session in [`Environment::game`](crate::Environment::game).

use std::time::Duration;

use ufathom_model::Dump;
use ufathom_model::layout::{Location, NameTableLayout, ObjectArrayKind, TransformOp};
use ufathom_runtime::{
    EngineDesc, Executor, FNameDesc, NameArrayDesc, NamePoolDesc, NamesDesc, ObjectArrayDesc,
    ObjectDesc, Place, Transform,
};

/// Work run on the game thread with an executor for calls.
pub type GameTask = Box<dyn FnOnce(&dyn Executor) + Send>;

/// Runs tasks on the game thread of the host process.
pub trait GameThreadRunner: Send + Sync {
    /// Prepares to run tasks for the build `dump` describes, for example by
    /// installing a hook. Called before the first [`run`](Self::run) of
    /// each preload; a later call for the same build may reuse what an
    /// earlier one set up.
    ///
    /// # Errors
    ///
    /// Why tasks cannot be run, as text for the dump log.
    fn attach(&self, dump: &Dump) -> Result<(), String>;

    /// Runs `task` on the game thread and waits until it has finished, or
    /// up to `timeout` for it to start and finish.
    ///
    /// # Errors
    ///
    /// Why the task did not run or did not finish in time.
    fn run(&self, task: GameTask, timeout: Duration) -> Result<(), String>;

    /// Removes what [`attach`](Self::attach) set up. Returns `true` when no
    /// thread is still running code of the host module, so it may unload.
    fn detach(&self, timeout: Duration) -> bool;
}

/// The runtime description of the build a dump was read from, for a
/// [`Session`](ufathom_runtime::Session) or an executor in this process.
///
/// The few strings and slices it holds are leaked: a description lives as
/// long as the hook that uses it, which is until the process ends.
#[must_use]
pub fn engine_desc(dump: &Dump) -> EngineDesc {
    let l = &dump.layout;
    let names = match &l.names {
        NameTableLayout::Pool(p) => NamesDesc::Pool(NamePoolDesc {
            location: place(&p.location),
            blocks: p.blocks,
            stride: p.stride,
            block_offset_bits: p.block_offset_bits,
            header_offset: p.entry.header_offset,
            string_offset: p.entry.string_offset,
            len_shift: p.entry.len_shift,
            wide_mask: p.entry.wide_mask,
        }),
        NameTableLayout::Array(a) => NamesDesc::Array(NameArrayDesc {
            location: place(&a.location),
            indirect: a.indirect,
            num_elements: a.num_elements,
            elements_per_chunk: a.elements_per_chunk,
            index_offset: a.entry.index_offset,
            string_offset: a.entry.string_offset,
            wide_mask: a.entry.wide_mask,
        }),
    };
    let o = &l.objects;
    let transform: Vec<Transform> = o.pointer_transform.iter().map(transform).collect();
    EngineDesc {
        names,
        objects: ObjectArrayDesc {
            location: place(&o.location),
            elements_per_chunk: match o.kind {
                ObjectArrayKind::Fixed => None,
                ObjectArrayKind::Chunked { elements_per_chunk } => Some(elements_per_chunk),
            },
            objects: o.objects,
            num_elements: o.num_elements,
            item_size: o.item_size,
            item_object: o.item_object,
            item_serial: o.item_serial,
            pointer_transform: Box::leak(transform.into_boxed_slice()),
        },
        fname: FNameDesc {
            size: l.fname.size,
            comparison_index: l.fname.comparison_index,
            display_index: l.fname.display_index,
            number: l.fname.number,
        },
        object: ObjectDesc {
            flags: l.uobject.flags,
            index: l.uobject.index,
            class: l.uobject.class,
            name: l.uobject.name,
            outer: l.uobject.outer,
            super_struct: l.ustruct.super_struct,
            properties_size: l.ustruct.properties_size,
            default_object: l.uclass.default_object,
        },
        process_event_index: dump.globals.process_event_index,
        gworld: dump.globals.gworld.as_ref().map(place),
        gengine: dump.globals.gengine.as_ref().map(place),
    }
}

fn place(location: &Location) -> Place {
    match location {
        Location::Rva { module, rva } => Place::Rva {
            module: Box::leak(module.clone().into_boxed_str()),
            rva: *rva,
        },
        Location::Absolute(addr) => Place::Absolute(*addr),
    }
}

fn transform(op: &TransformOp) -> Transform {
    match *op {
        TransformOp::Xor(v) => Transform::Xor(v),
        TransformOp::Add(v) => Transform::Add(v),
        TransformOp::Sub(v) => Transform::Sub(v),
        TransformOp::RotateLeft(n) => Transform::RotateLeft(n),
        TransformOp::RotateRight(n) => Transform::RotateRight(n),
        TransformOp::Not => Transform::Not,
    }
}
