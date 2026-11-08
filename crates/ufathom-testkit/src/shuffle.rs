//! Shuffled profiles and the validity check every profile must pass.
//!
//! Real engine builds reorder the `FUObjectArray` fields and put `Name` before
//! `Class`. A solver that only works on the stock order proves little, so
//! [`Profile::shuffled`] produces many different but always valid shapes.
//! Everything it picks is ASSUMED: none of it was measured on a real build.

use ufathom_model::layout::{
    FNameLayout, NameTableLayout, ObjectArrayKind, ObjectArrayLayout, PoolEntryLayout,
    PropertySystem, UEnumLayout, UObjectLayout,
};

use crate::profile::{EngineIdentity, Params, Profile, align_up, case_preserving_params, derive};
use crate::rng::Rng;

/// One field of a struct under validation: name, offset, length.
type Field<'a> = (&'a str, u32, u32);

/// Checks that `fields` lie inside `size` and do not overlap.
fn check_fields(owner: &str, size: u32, fields: &[Field<'_>]) -> Result<(), String> {
    for &(name, offset, len) in fields {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| format!("{owner}.{name} overflows"))?;
        if end > size {
            return Err(format!(
                "{owner}.{name} ends at {end:#x}, past the size {size:#x}"
            ));
        }
    }
    for (i, &(a, a_off, a_len)) in fields.iter().enumerate() {
        for &(b, b_off, b_len) in fields.iter().skip(i + 1) {
            if a_off < b_off + b_len && b_off < a_off + a_len {
                return Err(format!("{owner}.{a} overlaps {owner}.{b}"));
            }
        }
    }
    Ok(())
}

/// Picks one of the permutations of a few small items.
fn shuffled_indices(rng: &mut Rng, count: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    rng.shuffle(&mut order);
    order
}

/// Lays out `(size, align)` items in the given order from offset `start`.
/// Returns each item's offset (in the original item order) and the end offset.
fn pack(items: &[(u32, u32)], order: &[usize], start: u32) -> (Vec<u32>, u32) {
    let mut offsets = vec![0u32; items.len()];
    let mut cursor = start;
    for &index in order {
        let Some(&(size, align)) = items.get(index) else {
            continue;
        };
        cursor = align_up(cursor, align);
        if let Some(slot) = offsets.get_mut(index) {
            *slot = cursor;
        }
        cursor += size;
    }
    (offsets, cursor)
}

/// Looks an element up, returning 0 for a missing one. Used only on vectors
/// whose length is fixed by construction.
fn at(values: &[u32], index: usize) -> u32 {
    values.get(index).copied().unwrap_or(0)
}

fn shuffled_fname(rng: &mut Rng) -> FNameLayout {
    match rng.below(10) {
        0..=2 => FNameLayout {
            size: 12,
            comparison_index: 0,
            display_index: Some(4),
            number: Some(8),
        },
        3..=4 => FNameLayout {
            size: 12,
            comparison_index: 0,
            display_index: Some(8),
            number: Some(4),
        },
        5..=6 => FNameLayout {
            size: 8,
            comparison_index: 0,
            display_index: None,
            number: Some(4),
        },
        7 => FNameLayout {
            size: 16,
            comparison_index: 0,
            display_index: Some(4),
            number: Some(8),
        },
        // An outline-number build keeps the number outside the FName.
        8 => FNameLayout {
            size: 4,
            comparison_index: 0,
            display_index: None,
            number: None,
        },
        _ => FNameLayout {
            size: 8,
            comparison_index: 4,
            display_index: None,
            number: Some(0),
        },
    }
}

fn shuffled_uobject(rng: &mut Rng, fname: &FNameLayout) -> UObjectLayout {
    // vtable at 0; flags and index form an 8-byte pair that may swap.
    let (flags, index) = if rng.chance(30) { (0xC, 8) } else { (8, 0xC) };
    // class (8), name (fname), outer (8), in a random order, name often first.
    let items = [(8, 8), (fname.size, 4), (8, 8)];
    let mut order = shuffled_indices(rng, 3);
    if rng.chance(60) {
        order.sort_by_key(|&i| usize::from(i != 1));
    }
    let (offsets, end) = pack(&items, &order, 0x10);
    UObjectLayout {
        flags,
        index,
        class: at(&offsets, 0),
        name: at(&offsets, 1),
        outer: at(&offsets, 2),
        size: align_up(end, 8),
    }
}

struct ShuffledArray {
    layout: ObjectArrayLayout,
    prealloc: u32,
    size: u32,
    item_flags: u32,
    item_cluster_root: u32,
}

fn shuffled_object_array(rng: &mut Rng) -> ShuffledArray {
    // objects, prealloc, max_elements, num_elements, max_chunks, num_chunks
    let items = [(8, 8), (8, 8), (4, 4), (4, 4), (4, 4), (4, 4)];
    let order = shuffled_indices(rng, 6);
    let (offsets, end) = pack(&items, &order, 0);
    let (item_size, item_object, item_flags, item_cluster_root, item_serial) = match rng.below(3) {
        0 => (0x18, 0, 8, 0xC, 0x10),
        1 => (0x20, 0, 8, 0xC, 0x10),
        // UE 5.7 style: a 64-bit flags-and-refcount word comes first.
        _ => (0x18, 8, 0, 0x14, 0x10),
    };
    ShuffledArray {
        layout: ObjectArrayLayout {
            location: ufathom_model::layout::Location::Absolute(0),
            kind: ObjectArrayKind::Chunked {
                elements_per_chunk: 0x1_0000,
            },
            objects: at(&offsets, 0),
            max_elements: at(&offsets, 2),
            num_elements: at(&offsets, 3),
            max_chunks: Some(at(&offsets, 4)),
            num_chunks: Some(at(&offsets, 5)),
            item_size,
            item_object,
            item_serial: Some(item_serial),
            pointer_transform: Vec::new(),
        },
        prealloc: at(&offsets, 1),
        size: align_up(end, 8),
        item_flags,
        item_cluster_root,
    }
}

fn shuffled_pool(rng: &mut Rng, base: &NameTableLayout) -> NameTableLayout {
    let NameTableLayout::Pool(mut pool) = base.clone() else {
        return base.clone();
    };
    let (header_offset, string_offset, len_shift) = match rng.below(3) {
        0 => (4, 6, 1),
        1 => (0, 2, 6),
        _ => (0, 2, 1),
    };
    let strides: &[u32] = if header_offset == 4 {
        &[4, 8]
    } else {
        &[2, 4, 8]
    };
    pool.stride = rng.pick(strides).copied().unwrap_or(4);
    pool.block_offset_bits = rng.pick(&[15u32, 16, 17]).copied().unwrap_or(16);
    pool.entry = PoolEntryLayout {
        header_offset,
        string_offset,
        len_shift,
        wide_mask: 1,
    };
    NameTableLayout::Pool(pool)
}

impl Profile {
    /// A valid engine shape picked by `seed`, derived from the modern base.
    ///
    /// It reorders the `FUObjectArray` fields, moves `Name` before `Class` in
    /// `UObject`, varies the `FName` shape, the pool stride, the entry header
    /// shift and the item size, and sometimes uses split enum arrays, a
    /// 16-bit `MinAlignment`, doubles in `Vector`, an `FFieldClass` that
    /// starts with a vtable and a 64-bit `FField` owner. Every value is
    /// ASSUMED. The same seed always gives the same profile.
    ///
    /// # Panics
    ///
    /// If the picked shape fails [`Profile::validate`], which would be a bug in
    /// this function.
    #[must_use]
    pub fn shuffled(seed: u64) -> Self {
        let mut rng = Rng::new(seed ^ 0x5348_5546_464C_4544);
        let base = case_preserving_params(427);
        let fname = shuffled_fname(&mut rng);
        let uobject = shuffled_uobject(&mut rng, &fname);
        let array = shuffled_object_array(&mut rng);
        let params = Params {
            version: 427,
            names: shuffled_pool(&mut rng, &base.names),
            objects: array.layout,
            object_array_prealloc: Some(array.prealloc),
            object_array_size: array.size,
            item_flags: array.item_flags,
            item_cluster_root: Some(array.item_cluster_root),
            fname,
            uobject,
            ffield_owner_size: if rng.chance(40) { 8 } else { 16 },
            enum_split: rng.chance(30),
            min_alignment_width: if rng.chance(30) { 2 } else { 4 },
            vector_double: rng.chance(50),
            field_class_vtable: rng.chance(40),
            array_flags_first: rng.chance(50),
            process_event_index: u32::try_from(rng.range(0x30, 0x58)).unwrap_or(0x40),
        };
        let (layout, shape) = derive(&params);
        let identity = EngineIdentity {
            product_version: "5.0.0".to_owned(),
            changelist: u32::try_from(1000 + seed % 9000).unwrap_or(1000),
            branch: "++Shuffled+Test".to_owned(),
        };
        let profile = Self {
            name: format!("shuffled-{seed}"),
            identity,
            layout,
            shape,
        };
        if let Err(problem) = profile.validate() {
            panic!("shuffled profile {seed} is not a valid engine shape: {problem}");
        }
        profile
    }

    /// Checks that the profile describes a possible engine: every field fits
    /// inside its struct, nothing overlaps, alignments hold and every offset
    /// the writer relies on is consistent.
    ///
    /// # Errors
    ///
    /// A message naming the first violated rule.
    pub fn validate(&self) -> Result<(), String> {
        let layout = &self.layout;
        let shape = &self.shape;
        let fname = &layout.fname;

        if ![4, 8, 12, 16].contains(&fname.size) {
            return Err(format!("FName size {} is not 4, 8, 12 or 16", fname.size));
        }
        let mut fname_fields: Vec<Field<'_>> = vec![("comparison", fname.comparison_index, 4)];
        if let Some(display) = fname.display_index {
            fname_fields.push(("display", display, 4));
        }
        if let Some(number) = fname.number {
            fname_fields.push(("number", number, 4));
        }
        check_fields("FName", fname.size, &fname_fields)?;

        let obj = &layout.uobject;
        if obj.size % 8 != 0 {
            return Err("UObject size is not a multiple of 8".to_owned());
        }
        check_fields(
            "UObject",
            obj.size,
            &[
                ("vtable", 0, 8),
                ("flags", obj.flags, 4),
                ("index", obj.index, 4),
                ("class", obj.class, 8),
                ("name", obj.name, fname.size),
                ("outer", obj.outer, 8),
            ],
        )?;
        if [obj.class, obj.outer].iter().any(|offset| offset % 8 != 0) {
            return Err("UObject pointer is misaligned".to_owned());
        }
        if obj.name % 4 != 0 {
            return Err("UObject name is misaligned".to_owned());
        }

        let arr = &layout.objects;
        let mut array_fields: Vec<Field<'_>> = vec![
            ("objects", arr.objects, 8),
            ("max_elements", arr.max_elements, 4),
            ("num_elements", arr.num_elements, 4),
        ];
        match (
            &arr.kind,
            arr.max_chunks,
            arr.num_chunks,
            shape.object_array_prealloc,
        ) {
            (
                ObjectArrayKind::Chunked { .. },
                Some(max_chunks),
                Some(num_chunks),
                Some(prealloc),
            ) => {
                array_fields.push(("max_chunks", max_chunks, 4));
                array_fields.push(("num_chunks", num_chunks, 4));
                array_fields.push(("prealloc", prealloc, 8));
            }
            (ObjectArrayKind::Fixed, None, None, None) => {}
            _ => return Err("object array kind and its chunk fields disagree".to_owned()),
        }
        check_fields("FUObjectArray", shape.object_array_size, &array_fields)?;
        let Some(serial) = arr.item_serial else {
            return Err("the item must have a serial number".to_owned());
        };
        let mut item_fields: Vec<Field<'_>> = vec![
            ("object", arr.item_object, 8),
            ("flags", shape.item_flags, 4),
            ("serial", serial, 4),
        ];
        if let Some(cluster) = shape.item_cluster_root {
            item_fields.push(("cluster", cluster, 4));
        }
        check_fields("FUObjectItem", arr.item_size, &item_fields)?;

        match &layout.names {
            NameTableLayout::Pool(pool) => {
                if !pool.stride.is_power_of_two() {
                    return Err("pool stride is not a power of two".to_owned());
                }
                let entry = &pool.entry;
                if entry.header_offset + 2 > entry.string_offset {
                    return Err("pool entry header overlaps its characters".to_owned());
                }
                if entry.len_shift == 0 || entry.len_shift > 6 || entry.wide_mask != 1 {
                    return Err("pool entry header shift or wide mask is unusable".to_owned());
                }
                if pool.stride < 2 || (entry.header_offset >= 4 && pool.stride < 4) {
                    return Err("pool stride cannot hold the entry header".to_owned());
                }
                if pool.block_offset_bits + pool.stride.trailing_zeros() > 22 {
                    return Err("pool block would exceed 4 MiB".to_owned());
                }
                check_fields(
                    "FNameEntryAllocator",
                    pool.blocks + shape.name_pool_max_blocks * 8,
                    &[
                        ("current_block", pool.current_block, 4),
                        ("cursor", pool.cursor, 4),
                        ("blocks", pool.blocks, shape.name_pool_max_blocks * 8),
                    ],
                )?;
            }
            NameTableLayout::Array(array) => {
                let table = shape.name_array_max_chunks * 8;
                check_fields(
                    "TNameEntryArray",
                    table + 8,
                    &[
                        ("chunks", 0, table),
                        ("num_elements", array.num_elements, 4),
                        ("num_chunks", array.num_chunks, 4),
                    ],
                )?;
            }
        }

        let st = &layout.ustruct;
        let child_properties = st.child_properties.unwrap_or(st.children);
        let mut struct_fields: Vec<Field<'_>> = vec![
            ("super", st.super_struct, 8),
            ("children", st.children, 8),
            ("properties_size", st.properties_size, 4),
            ("min_alignment", st.min_alignment, st.min_alignment_width),
        ];
        if st.child_properties.is_some() {
            struct_fields.push(("child_properties", child_properties, 8));
        }
        let links = &shape.struct_links;
        struct_fields.extend([
            ("property_link", links.property_link, 8),
            ("ref_link", links.ref_link, 8),
            ("destructor_link", links.destructor_link, 8),
            ("post_construct_link", links.post_construct_link, 8),
        ]);
        if st.super_struct < shape.sizeof_ufield {
            return Err("UStruct::SuperStruct overlaps UField".to_owned());
        }
        check_fields("UStruct", shape.sizeof_ustruct, &struct_fields)?;

        let func = &layout.ufunction;
        if func.flags < shape.sizeof_ustruct {
            return Err("UFunction::FunctionFlags overlaps UStruct".to_owned());
        }
        if let Some(native) = func.native_func {
            check_fields(
                "UFunction",
                shape.sizeof_ufunction,
                &[("flags", func.flags, 4), ("func", native, 8)],
            )?;
        }
        if let (Some(cast), Some(cdo)) = (layout.uclass.cast_flags, layout.uclass.default_object) {
            if cast < shape.sizeof_ustruct {
                return Err("UClass::ClassCastFlags overlaps UStruct".to_owned());
            }
            check_fields(
                "UClass",
                shape.sizeof_uclass,
                &[("cast_flags", cast, 8), ("default_object", cdo, 8)],
            )?;
        }

        match &layout.uenum {
            UEnumLayout::PairArray {
                offset,
                stride,
                value_offset,
                value_width,
            } => {
                if *value_offset < fname.size || *stride < value_offset + value_width {
                    return Err("enum pair stride cannot hold a name and a value".to_owned());
                }
                check_fields("UEnum", shape.sizeof_uenum, &[("names", *offset, 16)])?;
            }
            UEnumLayout::SplitArrays {
                names,
                values,
                count,
            } => check_fields(
                "UEnum",
                shape.sizeof_uenum,
                &[
                    ("names", *names, 8),
                    ("values", *values, 8),
                    ("count", *count, 4),
                ],
            )?,
        }

        let props = &layout.properties;
        let base_size = match &props.system {
            PropertySystem::Legacy => shape.sizeof_ufield,
            PropertySystem::Modern {
                ffield,
                field_class,
            } => {
                let (Some(fs), Some(fc), Some(flags), Some(cast)) = (
                    shape.ffield.as_ref(),
                    shape.field_class.as_ref(),
                    ffield.flags,
                    field_class.cast_flags,
                ) else {
                    return Err("a modern profile needs FField and FFieldClass extras".to_owned());
                };
                let mut ffield_fields: Vec<Field<'_>> = vec![
                    ("vtable", 0, 8),
                    ("class", ffield.class, 8),
                    ("owner", fs.owner, 8),
                    ("next", ffield.next, 8),
                    ("name", ffield.name, fname.size),
                    ("flags", flags, 4),
                ];
                if let Some(is_uobject) = fs.owner_is_uobject {
                    ffield_fields.push(("owner_is_uobject", is_uobject, 1));
                }
                check_fields("FField", fs.size, &ffield_fields)?;
                check_fields(
                    "FFieldClass",
                    fc.size,
                    &[
                        ("name", field_class.name, fname.size),
                        ("id", fc.id, 8),
                        ("cast_flags", cast, 8),
                        ("class_flags", fc.class_flags, 4),
                        ("super_class", fc.super_class, 8),
                        ("default_object", fc.default_object, 8),
                        ("construct_fn", fc.construct_fn, 8),
                    ],
                )?;
                fs.size
            }
        };
        let next = &shape.property_links;
        check_fields(
            "FProperty",
            props.size,
            &[
                ("array_dim", props.array_dim, 4),
                ("element_size", props.element_size, 4),
                ("flags", props.flags, 8),
                ("offset_internal", props.offset_internal, 4),
                ("property_next", next.property_next, 8),
                ("ref_next", next.ref_next, 8),
                ("destructor_next", next.destructor_next, 8),
                ("post_construct_next", next.post_construct_next, 8),
            ],
        )?;
        if props.array_dim < base_size {
            return Err("property members overlap the field base".to_owned());
        }
        let typed = &props.typed;
        for (name, offset) in [
            ("object_class", typed.object_class),
            ("class_meta", typed.class_meta),
            ("script_struct", typed.script_struct),
            ("array_inner", typed.array_inner),
            ("map_value", typed.map_value),
        ] {
            if offset.is_none_or(|value| value < props.size) {
                return Err(format!(
                    "typed payload {name} is missing or inside the base"
                ));
            }
        }
        if shape.process_event_index + 1 >= shape.vtable_slots {
            return Err("ProcessEvent index is outside the vtable".to_owned());
        }
        Ok(())
    }
}
