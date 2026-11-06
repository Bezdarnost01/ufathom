//! Corruption injection: damage a finished image in one specific way.
//!
//! Every variant documents what a robust reader should do. In all cases the
//! reader must not panic, hang or read unbounded memory, and must report the
//! problem with the path or address involved.

use ufathom_model::layout::{NameTableLayout, ObjectArrayKind, PropertySystem, UEnumLayout};

use crate::data::DataAnchors;
use crate::names::NameTable;
use crate::profile::Profile;
use crate::reflect::Reflection;
use crate::rng::Rng;
use crate::space::Mem;

/// One way to damage an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Corruption {
    /// About a twentieth of the filler slots, and the `Default__Pawn` slot,
    /// hold a null object pointer.
    ///
    /// A reader skips null slots and counts them. It must not treat a null
    /// pointer as an object, and the remaining objects stay readable.
    NullObjectSlots,
    /// The `Actor` and `Pawn` classes name each other as their class, so the
    /// class chain of every `Actor` or `Pawn` instance loops and never reaches
    /// a fixed point.
    ///
    /// A reader bounds class-chain walks and reports the object whose chain
    /// does not terminate. Other objects are unaffected.
    ClassChainCycle,
    /// `Actor::SuperStruct` points at `Pawn`, whose super is `Actor`.
    ///
    /// A reader detects the loop while walking supers, reports the structs
    /// involved, and does not loop forever or recurse without bound.
    SuperChainCycle,
    /// The member chain of the `Guid` struct ends by pointing back at its
    /// first member.
    ///
    /// A reader detects the cycle, keeps the members seen once and reports the
    /// struct. It must not emit duplicate fields.
    PropertyListCycle,
    /// The names arrays of all enums claim `i32::MAX` elements.
    ///
    /// A reader checks the count against a named limit before allocating or
    /// reading, and reports the enum.
    HugeArrayCounts,
    /// `Actor`'s member list, `Pawn`'s super, `Level`'s outer and one property's
    /// class point into unmapped memory.
    ///
    /// A reader treats the failed read as that object's error, not the end of
    /// the dump.
    DanglingPointers,
    /// The pointer to the last name block (or chunk) points into unmapped
    /// memory.
    ///
    /// Names stored there fail to decode. A reader reports them and continues
    /// with the names it can read.
    UnmappedNamesBlock,
    /// The names of `Actor`, `Pawn`, `K2_DestroyActor` and one filler hold an
    /// index far past the end of the name table.
    ///
    /// A reader reports an unresolvable name for those objects, does not index
    /// out of bounds, and does not invent a name.
    NameIndexOutOfRange,
    /// One ASCII name holds a Latin-1 byte (not valid UTF-8) and one wide name
    /// holds a lone surrogate (not valid UTF-16).
    ///
    /// A reader decodes the first as Latin-1 and the second lossily. Neither
    /// is an error and neither panics.
    NonUtf8Names,
    /// The object array claims one chunk more than its chunk table holds, and
    /// the table ends at a page boundary so the extra entry is unmapped. For a
    /// fixed array, the element count exceeds the allocation.
    ///
    /// A reader checks the chunk count against the chunk capacity and refuses
    /// to read past it.
    TruncatedChunkTable,
    /// The headers of the `Pawn` class and about 25 filler objects are zero:
    /// no vtable, class, name or outer.
    ///
    /// A reader rejects an object whose header does not validate and moves on.
    ZeroedObjectHeaders,
    /// `Guid::B` reports an element size of 0x100, larger than the struct, and
    /// `Actor::NetTag` reports 0.
    ///
    /// A reader reports that the member does not fit in its owner. It must not
    /// derive a layout from the bad size.
    WrongElementSize,
    /// `Guid::D` and `Color::G` report offsets that overlap an earlier member.
    ///
    /// A reader reports the overlap, and a generator degrades the struct to
    /// raw bytes.
    OverlappingField,
    /// One filler slot's object memory was freed and reused: it now holds the
    /// header of the object from a different slot, and its serial changed.
    ///
    /// A reader's re-validation notices that the object stores another index,
    /// retries once and records a diagnostic.
    ObjectFreedAndReused,
}

impl Corruption {
    /// Every corruption, in declaration order.
    pub const ALL: [Self; 14] = [
        Self::NullObjectSlots,
        Self::ClassChainCycle,
        Self::SuperChainCycle,
        Self::PropertyListCycle,
        Self::HugeArrayCounts,
        Self::DanglingPointers,
        Self::UnmappedNamesBlock,
        Self::NameIndexOutOfRange,
        Self::NonUtf8Names,
        Self::TruncatedChunkTable,
        Self::ZeroedObjectHeaders,
        Self::WrongElementSize,
        Self::OverlappingField,
        Self::ObjectFreedAndReused,
    ];
}

/// Everything a corruption needs to find its targets.
pub(crate) struct Context<'a> {
    pub profile: &'a Profile,
    pub reflection: &'a Reflection,
    pub data: &'a DataAnchors,
    pub names: &'a NameTable,
    pub unmapped: u64,
}

/// A name comparison index far outside any table.
const BAD_NAME_INDEX: u32 = 0x7FFF_FFF0;

impl Context<'_> {
    fn addr(&self, path: &str) -> u64 {
        self.reflection.addr.get(path).copied().unwrap_or(0)
    }

    fn index_of(&self, path: &str) -> Option<usize> {
        self.reflection
            .records
            .iter()
            .find(|r| r.path == path)
            .map(|r| usize::try_from(r.index).unwrap_or(0))
    }

    /// Address of the item for slot `index`.
    fn item(&self, index: usize) -> u64 {
        let layout = &self.profile.layout.objects;
        let size = u64::from(layout.item_size);
        match layout.kind {
            ObjectArrayKind::Fixed => self.data.items.table + index as u64 * size,
            ObjectArrayKind::Chunked { elements_per_chunk } => {
                let per = usize::try_from(elements_per_chunk).unwrap_or(1);
                let chunk = self
                    .data
                    .items
                    .chunks
                    .get(index / per)
                    .copied()
                    .unwrap_or(0);
                chunk + (index % per) as u64 * size
            }
        }
    }

    /// Filler-like actors: the two special objects and every filler instance.
    fn actors(&self) -> Vec<usize> {
        self.reflection
            .records
            .iter()
            .filter(|r| r.path.starts_with("/Game/Maps/Main.Main:PersistentLevel."))
            .map(|r| usize::try_from(r.index).unwrap_or(0))
            .collect()
    }

    fn object_at(&self, index: usize) -> u64 {
        self.reflection.slots.get(index).map_or(0, |s| s.object)
    }
}

fn walk_chain(mem: &mut Mem, head: u64, next: u64) -> Vec<u64> {
    let mut chain = Vec::new();
    let mut at = head;
    while at != 0 && chain.len() < 64 && !chain.contains(&at) {
        chain.push(at);
        at = mem.get_u64(at + next);
    }
    chain
}

/// Applies one corruption.
pub(crate) fn apply(mem: &mut Mem, ctx: &Context<'_>, corruption: Corruption, rng: &mut Rng) {
    let layout = &ctx.profile.layout;
    let uobject = &layout.uobject;
    match corruption {
        Corruption::NullObjectSlots => {
            let mut victims: Vec<usize> = ctx.actors().into_iter().step_by(20).collect();
            victims.extend(ctx.index_of("/Script/Engine.Default__Pawn"));
            for index in victims {
                mem.put_u64(ctx.item(index) + u64::from(layout.objects.item_object), 0);
            }
        }
        Corruption::ClassChainCycle => {
            let actor = ctx.addr("/Script/Engine.Actor");
            let pawn = ctx.addr("/Script/Engine.Pawn");
            mem.put_u64(actor + u64::from(uobject.class), pawn);
            mem.put_u64(pawn + u64::from(uobject.class), actor);
        }
        Corruption::SuperChainCycle => {
            let actor = ctx.addr("/Script/Engine.Actor");
            let pawn = ctx.addr("/Script/Engine.Pawn");
            mem.put_u64(actor + u64::from(layout.ustruct.super_struct), pawn);
        }
        Corruption::PropertyListCycle => {
            let guid = ctx.addr("/Script/CoreUObject.Guid");
            let (head, next) = match (&layout.properties.system, layout.ustruct.child_properties) {
                (PropertySystem::Modern { ffield, .. }, Some(child_properties)) => (
                    mem.get_u64(guid + u64::from(child_properties)),
                    u64::from(ffield.next),
                ),
                _ => (
                    mem.get_u64(guid + u64::from(layout.ustruct.children)),
                    u64::from(layout.ufield.next),
                ),
            };
            let chain = walk_chain(mem, head, next);
            if let (Some(&first), Some(&last)) = (chain.first(), chain.last()) {
                mem.put_u64(last + next, first);
            }
        }
        Corruption::HugeArrayCounts => {
            for def in &ctx.reflection.schema.enums {
                let a = ctx.addr(&def.path);
                match layout.uenum {
                    UEnumLayout::PairArray { offset, .. } => {
                        mem.put_u32(a + u64::from(offset) + 8, i32::MAX as u32);
                        mem.put_u32(a + u64::from(offset) + 12, i32::MAX as u32);
                    }
                    UEnumLayout::SplitArrays { count, .. } => {
                        mem.put_u32(a + u64::from(count), i32::MAX as u32);
                    }
                }
            }
        }
        Corruption::DanglingPointers => {
            let actor = ctx.addr("/Script/Engine.Actor");
            let list = layout
                .ustruct
                .child_properties
                .unwrap_or(layout.ustruct.children);
            mem.put_u64(actor + u64::from(list), ctx.unmapped);
            let pawn = ctx.addr("/Script/Engine.Pawn");
            mem.put_u64(pawn + u64::from(layout.ustruct.super_struct), ctx.unmapped);
            let level = ctx.addr("/Script/Engine.Level");
            mem.put_u64(level + u64::from(uobject.outer), ctx.unmapped);
            let property = ctx.addr("/Script/Engine.Actor:bHidden");
            match &layout.properties.system {
                PropertySystem::Modern { ffield, .. } => {
                    mem.put_u64(property + u64::from(ffield.class), ctx.unmapped);
                }
                PropertySystem::Legacy => {
                    mem.put_u64(property + u64::from(uobject.class), ctx.unmapped)
                }
            }
        }
        Corruption::UnmappedNamesBlock => {
            if let Some(&slot) = ctx.data.names.block_slots.last() {
                mem.put_u64(slot, ctx.unmapped);
            }
        }
        Corruption::NameIndexOutOfRange => {
            let bad = match &layout.names {
                NameTableLayout::Pool(pool) => (7000u32 << pool.block_offset_bits) | 5,
                NameTableLayout::Array(_) => BAD_NAME_INDEX,
            };
            let mut victims = vec![
                ctx.addr("/Script/Engine.Actor"),
                ctx.addr("/Script/Engine.Pawn"),
                ctx.addr("/Script/Engine.Actor:K2_DestroyActor"),
            ];
            victims.extend(ctx.actors().first().map(|&i| ctx.object_at(i)));
            for victim in victims {
                let name = victim + u64::from(uobject.name);
                mem.put_u32(name + u64::from(layout.fname.comparison_index), bad);
                if let Some(display) = layout.fname.display_index {
                    mem.put_u32(name + u64::from(display), bad);
                }
            }
        }
        Corruption::NonUtf8Names => {
            let special = &ctx.reflection.special_names;
            let string_offset = u64::from(ctx.names.string_offset());
            if let Some(entry) = ctx
                .names
                .ordinal_of(&special.ascii)
                .and_then(|n| ctx.data.names.entries.get(n))
            {
                // "CafeSpawn": the fourth character becomes Latin-1 e-acute.
                mem.put_u8(entry + string_offset + 3, 0xE9);
            }
            if let Some(entry) = ctx
                .names
                .ordinal_of(&special.wide)
                .and_then(|n| ctx.data.names.entries.get(n))
            {
                // The third UTF-16 unit becomes a lone high surrogate.
                mem.put_u16(entry + string_offset + 4, 0xD800);
            }
        }
        Corruption::TruncatedChunkTable => {
            let array = ctx.data.object_array;
            let objects = &layout.objects;
            match (objects.max_chunks, objects.num_chunks) {
                (Some(max_chunks), Some(num_chunks)) => {
                    let capacity = ctx.data.items.max_chunks;
                    mem.put_u32(array + u64::from(num_chunks), capacity + 1);
                    let _ = max_chunks;
                }
                _ => {
                    let past = ctx.profile.shape.object_array_capacity + 0x1000;
                    mem.put_u32(array + u64::from(objects.num_elements), past);
                }
            }
        }
        Corruption::ZeroedObjectHeaders => {
            let mut victims = vec![ctx.addr("/Script/Engine.Pawn")];
            let actors = ctx.actors();
            let step = (actors.len() / 25).max(1);
            victims.extend(
                actors
                    .iter()
                    .step_by(step)
                    .take(25)
                    .map(|&i| ctx.object_at(i)),
            );
            for victim in victims {
                let zeros = vec![0u8; usize::try_from(uobject.size).unwrap_or(0)];
                mem.put(victim, &zeros);
            }
        }
        Corruption::WrongElementSize => {
            let props = &layout.properties;
            mem.put_u32(
                ctx.addr("/Script/CoreUObject.Guid:B") + u64::from(props.element_size),
                0x100,
            );
            mem.put_u32(
                ctx.addr("/Script/Engine.Actor:NetTag") + u64::from(props.element_size),
                0,
            );
        }
        Corruption::OverlappingField => {
            let props = &layout.properties;
            mem.put_u32(
                ctx.addr("/Script/CoreUObject.Guid:D") + u64::from(props.offset_internal),
                4,
            );
            mem.put_u32(
                ctx.addr("/Script/CoreUObject.Color:G") + u64::from(props.offset_internal),
                0,
            );
        }
        Corruption::ObjectFreedAndReused => {
            let actors = ctx.actors();
            if actors.len() >= 4 {
                let pick = |rng: &mut Rng| {
                    actors
                        .get(usize::try_from(rng.below(actors.len() as u64)).unwrap_or(0))
                        .copied()
                        .unwrap_or(0)
                };
                let victim = pick(rng);
                let mut donor = pick(rng);
                if donor == victim {
                    donor = actors
                        .iter()
                        .copied()
                        .find(|&i| i != victim)
                        .unwrap_or(victim);
                }
                let header = mem.get(
                    ctx.object_at(donor),
                    usize::try_from(uobject.size).unwrap_or(0),
                );
                mem.put(ctx.object_at(victim), &header);
                let serial = layout.objects.item_serial.unwrap_or(0x10);
                let at = ctx.item(victim) + u64::from(serial);
                let old = mem.get_u32(at);
                mem.put_u32(at, old.wrapping_add(1000));
            }
        }
    }
}
