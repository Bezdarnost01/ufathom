//! Engine profiles: the layout an image is written with and the solver is
//! expected to find again.
//!
//! A profile is derived from an engine version and a few build options. The
//! derivation follows the public per-version type descriptions of the
//! MIT-licensed `jmap` project (see `THIRD_PARTY.md`), written down as
//! formulas in [`derive`]. The variant constructors
//! ([`Profile::ue4_21_measured`] and friends) name builds whose layout was
//! measured on real executables; a test checks them against the measured
//! numbers exactly.
//!
//! # Measured, inferred and assumed values
//!
//! Only the values listed on a `*_measured` constructor were measured. A value
//! derived from a measured one is called *inferred*. Everything else is
//! **assumed**: derived from `jmap`, not measured. Assumed values carry an
//! `ASSUMED` comment where they are produced, so the two can never be confused.
//! A solver passing against an assumed value proves nothing about a real game.

use std::fmt;

use ufathom_model::layout::{
    BoolPayloadLayout, FFieldClassLayout, FFieldLayout, FNameLayout, Location, NameArrayLayout,
    NameEntryArrayLayout, NamePoolLayout, NameTableLayout, ObjectArrayKind, ObjectArrayLayout,
    PoolEntryLayout, PropertyLayout, PropertySystem, TypedPayloadLayout, UClassLayout, UEnumLayout,
    UFieldLayout, UFunctionLayout, UObjectLayout, UStructLayout,
};
use ufathom_model::{EngineLayout, EngineVersion};

/// Name and version strings an image carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineIdentity {
    /// Version written to `VS_VERSIONINFO` `ProductVersion`, such as `4.25.1`.
    pub product_version: String,
    /// Changelist embedded in the build string. Always invented.
    pub changelist: u32,
    /// Branch part of the build string, such as `++Game+Live`.
    pub branch: String,
}

impl EngineIdentity {
    /// The UTF-16 build string stored in `.rdata`: `<branch>-CL-<changelist>`.
    #[must_use]
    pub fn build_string(&self) -> String {
        format!("{}-CL-{}", self.branch, self.changelist)
    }

    /// Splits `product_version` into `(major, minor, patch)`.
    #[must_use]
    pub fn version_parts(&self) -> (u16, u16, u16) {
        let mut parts = self
            .product_version
            .split('.')
            .map(|part| part.parse::<u16>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    }
}

/// Offsets of `FField` that [`EngineLayout`] has no field for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FFieldShape {
    /// Offset of `Owner` (`FFieldVariant`).
    pub owner: u32,
    /// Offset of the `bIsUObject` byte. `None` when the owner is a bare union.
    pub owner_is_uobject: Option<u32>,
    /// `sizeof(FField)`.
    pub size: u32,
}

/// Offsets of `FFieldClass` that [`EngineLayout`] has no field for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldClassShape {
    /// Offset of `Id`.
    pub id: u32,
    /// Offset of `ClassFlags`.
    pub class_flags: u32,
    /// Offset of `SuperClass`.
    pub super_class: u32,
    /// Offset of `DefaultObject`.
    pub default_object: u32,
    /// Offset of `ConstructFn`.
    pub construct_fn: u32,
    /// `sizeof(FFieldClass)`.
    pub size: u32,
}

/// Facts the writer needs that [`EngineLayout`] cannot express. All of them
/// are ASSUMED for every profile (derived, not measured) unless a constructor
/// says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// The engine version as `major * 100 + minor` (425 for 4.25, 503 for
    /// 5.3), the number `jmap` calls `UE_VERSION`.
    pub version: u32,
    /// `sizeof(UField)`.
    pub sizeof_ufield: u32,
    /// `sizeof(UStruct)`.
    pub sizeof_ustruct: u32,
    /// `sizeof(UClass)`.
    pub sizeof_uclass: u32,
    /// `sizeof(UFunction)`.
    pub sizeof_ufunction: u32,
    /// `sizeof(UScriptStruct)`.
    pub sizeof_uscriptstruct: u32,
    /// `sizeof(UEnum)`.
    pub sizeof_uenum: u32,
    /// `sizeof(UPackage)`. Approximate: the real class is large and irrelevant here.
    pub sizeof_upackage: u32,
    /// Offset of `NumParms` from `FunctionFlags`: 4, or 6 before UE 4.18 where
    /// `RepOffset` precedes it. `ParmsSize` and `ReturnValueOffset` follow.
    pub ufunction_parm_block: u32,
    /// Offset of `PreAllocatedObjects` inside the chunked object array.
    /// `None` for the fixed array.
    pub object_array_prealloc: Option<u32>,
    /// `sizeof` of the object array structure.
    pub object_array_size: u32,
    /// Offset of the internal flags field inside a `FUObjectItem`.
    pub item_flags: u32,
    /// Offset of the cluster root index inside a `FUObjectItem`. `None` when
    /// it shares a field with the flags (before UE 4.13).
    pub item_cluster_root: Option<u32>,
    /// Virtual function table index of `UObject::ProcessEvent`.
    pub process_event_index: u32,
    /// Number of slots written for every vtable.
    pub vtable_slots: u32,
    /// `Vector`, `Rotator` and `Quat` hold doubles (UE5 large world coordinates).
    pub vector_double: bool,
    /// Class name of an inline multicast delegate property.
    pub multicast_inline_class: String,
    /// `true` before UE 4.15, where soft pointers were `AssetObjectProperty`
    /// and `AssetClassProperty`.
    pub legacy_asset_names: bool,
    /// `false` before UE 4.15, which has neither `EnumProperty` nor `SetProperty`.
    pub has_enum_and_set_properties: bool,
    /// Offset of `CppType` (an `FString`) inside `UEnum`.
    pub enum_cpp_type: u32,
    /// `FField` extras. `None` for the legacy property system.
    pub ffield: Option<FFieldShape>,
    /// `FFieldClass` extras. `None` for the legacy property system.
    pub field_class: Option<FieldClassShape>,
    /// Maximum number of `FNamePool` blocks.
    pub name_pool_max_blocks: u32,
    /// Maximum number of chunks of a `TNameEntryArray`.
    pub name_array_max_chunks: u32,
    /// Capacity of the object array, as written to `MaxElements`.
    pub object_array_capacity: u32,
}

/// A complete description of one engine build: what to write and what a
/// solver is expected to find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Short identifier such as `ue4.25`, `ue4.25-cp` or `shuffled-7`. Used as
    /// the command line name of the fake host.
    pub name: String,
    /// Version strings.
    pub identity: EngineIdentity,
    /// The expected solver answer. Locations are placeholders
    /// (`Absolute(0)`); the builder fills them in.
    pub layout: EngineLayout,
    /// Extra facts needed to write bytes.
    pub shape: Shape,
}

/// The format of one `FNamePool` entry, where builds differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolEntryFormat {
    /// The entry starts with a 4-byte comparison id (case-preserving builds),
    /// so the header sits at +4 and the characters at +6. Otherwise they sit
    /// at +0 and +2.
    pub comparison_id: bool,
    /// Right shift that extracts the length from the header: 1 when the header
    /// is `wide | len << 1`, 6 when a 5-bit probe hash sits in between.
    pub len_shift: u32,
}

/// Build variations of one engine version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuildOptions {
    /// `WITH_CASE_PRESERVING_NAME`: `FName` carries a display index next to the
    /// comparison index (12 bytes). Only valid from UE 4.22, where the display
    /// index appeared.
    pub case_preserving_names: bool,
    /// Overrides the pool entry format. `None` picks the default for the
    /// version and the case-preserving option. Only valid from UE 4.23, where
    /// the pool appeared.
    pub pool_entry: Option<PoolEntryFormat>,
}

/// Why a profile could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    /// The version is outside the supported UE 4.11 to 5.8 range.
    UnsupportedVersion(EngineVersion),
    /// Case-preserving names before UE 4.22.
    CasePreservingBeforeDisplayIndex(EngineVersion),
    /// A pool entry format for a version without `FNamePool`.
    PoolEntryWithoutPool(EngineVersion),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "engine version {v} is outside the supported range 4.11 to 5.8"
                )
            }
            Self::CasePreservingBeforeDisplayIndex(v) => {
                write!(
                    f,
                    "engine version {v} has no FName display index (added in 4.22)"
                )
            }
            Self::PoolEntryWithoutPool(v) => {
                write!(f, "engine version {v} has no FNamePool (added in 4.23)")
            }
        }
    }
}

impl std::error::Error for ProfileError {}

/// Placeholder for a location the builder fills in.
const UNPLACED: Location = Location::Absolute(0);

/// Number of `FNamePool` blocks: `FNameMaxBlockBits` = 13. ASSUMED from the
/// engine source layout, consistent with a measured `blocks +0x10`.
const POOL_MAX_BLOCKS: u32 = 8192;

/// `TNameEntryArray` holds 128 chunks of 16384 entries (2M names). ASSUMED.
const NAME_ARRAY_CHUNKS: u32 = 128;

/// Entries in one `TNameEntryArray` chunk. ASSUMED.
const NAME_ARRAY_PER_CHUNK: u32 = 16384;

/// Default capacity of the chunked object array: 2 Mi objects. ASSUMED.
const OBJECT_CAPACITY: u32 = 2 * 1024 * 1024;

/// Capacity of the fixed object array (`MaxObjectsInGame`). ASSUMED.
const FIXED_OBJECT_CAPACITY: u32 = 0x1_0000;

/// Items per chunk of the object array. MEASURED for the builds listed on the
/// measured constructors (0x10000).
const OBJECT_ITEMS_PER_CHUNK: u32 = 0x1_0000;

/// The first version with a chunked object array (`jmap`: `UE_VERSION < 420`
/// is the fixed array).
const FIRST_CHUNKED: u32 = 420;
/// The first version with `FNamePool`.
const FIRST_POOL: u32 = 423;
/// The first version with `FField` properties.
const FIRST_FFIELD: u32 = 425;

/// Rounds `value` up to a multiple of `align` (a power of two).
#[must_use]
pub const fn align_up(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

/// A cursor that places fields at their natural alignment.
struct Cursor(u32);

impl Cursor {
    fn place(&mut self, size: u32, align: u32) -> u32 {
        self.0 = align_up(self.0, align);
        let offset = self.0;
        self.0 += size;
        offset
    }
}

/// Parameters of [`derive`]. Everything not listed follows from `version`.
#[derive(Debug, Clone)]
pub struct Params {
    /// `UE_VERSION` in `jmap`'s sense: `major * 100 + minor`.
    pub version: u32,
    /// `FName` shape.
    pub fname: FNameLayout,
    /// `UObject` offsets.
    pub uobject: UObjectLayout,
    /// Name table.
    pub names: NameTableLayout,
    /// Object array.
    pub objects: ObjectArrayLayout,
    /// Offset of `PreAllocatedObjects`; `None` for the fixed array.
    pub object_array_prealloc: Option<u32>,
    /// `sizeof` of the array structure.
    pub object_array_size: u32,
    /// Offset of the internal flags in an item.
    pub item_flags: u32,
    /// Offset of the cluster root in an item.
    pub item_cluster_root: Option<u32>,
    /// `UEnum` stores split arrays (UE 5.7).
    pub enum_split: bool,
    /// Width of `MinAlignment`: 4, or 2 from UE 5.6.
    pub min_alignment_width: u32,
    /// `Vector` holds doubles.
    pub vector_double: bool,
    /// `FFieldClass` starts with a vtable pointer, shifting the name to +8.
    pub field_class_vtable: bool,
    /// `ArrayFlags` precede `Inner` (UE 5.3).
    pub array_flags_first: bool,
    /// Size of `FFieldVariant`: 16 with the `bIsUObject` byte, 8 without.
    pub ffield_owner_size: u32,
    /// ProcessEvent vtable index.
    pub process_event_index: u32,
}

fn standard_uobject(fname_size: u32) -> UObjectLayout {
    let outer = align_up(0x18 + fname_size, 8);
    UObjectLayout {
        flags: 8,
        index: 0xC,
        class: 0x10,
        name: 0x18,
        outer,
        size: outer + 8,
    }
}

/// The name table of a version. ASSUMED from `jmap` and the engine source:
/// `TNameEntryArray` of 128 chunks before 4.23 (entry index at +0 with bit 0
/// the wide flag, hash chain at +8, characters at +0x10), `FNamePool` from 4.23
/// (blocks at +0x10, current block +8, cursor +0xC, stride 4, 16 offset bits).
fn names_for(version: u32, entry: PoolEntryFormat) -> NameTableLayout {
    if version < FIRST_POOL {
        return NameTableLayout::Array(NameArrayLayout {
            location: UNPLACED,
            indirect: true,
            num_elements: NAME_ARRAY_CHUNKS * 8,
            num_chunks: NAME_ARRAY_CHUNKS * 8 + 4,
            elements_per_chunk: NAME_ARRAY_PER_CHUNK,
            entry: NameEntryArrayLayout {
                index_offset: 0,
                string_offset: 0x10,
                wide_mask: 1,
            },
        });
    }
    let header_offset = if entry.comparison_id { 4 } else { 0 };
    NameTableLayout::Pool(NamePoolLayout {
        location: UNPLACED,
        blocks: 0x10,
        current_block: 8,
        cursor: 0xC,
        stride: 4,
        block_offset_bits: 16,
        entry: PoolEntryLayout {
            header_offset,
            string_offset: header_offset + 2,
            len_shift: entry.len_shift,
            wide_mask: 1,
        },
    })
}

/// The object array of a version, from `jmap`'s `FUObjectArray` and
/// `FUObjectItem`. ASSUMED. The location of the array is the start of the
/// `ObjObjects` member, not of the surrounding `FUObjectArray`.
fn object_array_for(version: u32) -> (ObjectArrayLayout, Option<u32>, u32, u32, Option<u32>) {
    // (item_size, item_object, item_flags, item_cluster_root, item_serial)
    let (item_size, item_object, item_flags, item_cluster, item_serial) = if version < 413 {
        (0x10, 0, 8, None, 0xC)
    } else if version < 507 {
        (0x18, 0, 8, Some(0xC), 0x10)
    } else {
        // A 64-bit flags-and-refcount word comes first.
        (0x18, 8, 0, Some(0x14), 0x10)
    };
    let items = |kind: ObjectArrayKind, fields: [u32; 6]| ObjectArrayLayout {
        location: UNPLACED,
        kind,
        objects: fields[0],
        max_elements: fields[2],
        num_elements: fields[3],
        max_chunks: Some(fields[4]),
        num_chunks: Some(fields[5]),
        item_size,
        item_object,
        item_serial: Some(item_serial),
        pointer_transform: Vec::new(),
    };
    let chunked = ObjectArrayKind::Chunked {
        elements_per_chunk: OBJECT_ITEMS_PER_CHUNK,
    };
    if version < FIRST_CHUNKED {
        // FFixedUObjectArray: Objects, MaxElements, NumElements.
        let layout = ObjectArrayLayout {
            location: UNPLACED,
            kind: ObjectArrayKind::Fixed,
            objects: 0,
            max_elements: 8,
            num_elements: 0xC,
            max_chunks: None,
            num_chunks: None,
            item_size,
            item_object,
            item_serial: Some(item_serial),
            pointer_transform: Vec::new(),
        };
        return (layout, None, 0x10, item_flags, item_cluster);
    }
    if version >= 508 {
        // Objects, NumElements, MaxElements, NumChunks, MaxChunks, PreAllocated.
        let layout = items(chunked, [0, 0x18, 0xC, 8, 0x14, 0x10]);
        return (layout, Some(0x18), 0x20, item_flags, item_cluster);
    }
    // Objects, PreAllocated, MaxElements, NumElements, MaxChunks, NumChunks.
    let layout = items(chunked, [0, 8, 0x10, 0x14, 0x18, 0x1C]);
    (layout, Some(8), 0x20, item_flags, item_cluster)
}

/// Typed payload offsets for a property object whose base ends at `size`.
/// ASSUMED beyond the measured 4.22 values (which follow this rule).
#[must_use]
pub fn typed_payloads(size: u32, array_flags_first: bool) -> TypedPayloadLayout {
    TypedPayloadLayout {
        bool_property: Some(BoolPayloadLayout {
            field_size: size,
            byte_offset: size + 1,
            byte_mask: size + 2,
            field_mask: size + 3,
        }),
        object_class: Some(size),
        class_meta: Some(size + 8),
        script_struct: Some(size),
        array_inner: Some(if array_flags_first { size + 8 } else { size }),
        set_element: Some(size),
        map_key: Some(size),
        map_value: Some(size + 8),
        enum_underlying: Some(size),
        enum_type: Some(size + 8),
        byte_enum: Some(size),
        delegate_signature: Some(size),
        interface_class: Some(size),
        optional_value: None,
    }
}

/// Offsets of the `UProperty` / `FProperty` members, from `jmap`.
/// Returns `(array_dim, element_size, flags, offset_internal, size)`.
fn property_members(version: u32, base_end: u32, fname: u32) -> (u32, u32, u32, u32, u32) {
    // FName is 8-aligned in 4.11 to 4.21 (it held a union with a 64-bit value).
    let fname_align = if (411..422).contains(&version) { 8 } else { 4 };
    let array_dim = base_end;
    let element_size = array_dim + 4;
    let flags = element_size + 4;
    let rep_index = flags + 8;
    let mut c = Cursor(rep_index + 2);
    let offset_internal;
    if version < 418 {
        c.place(fname, fname_align); // RepNotifyFunc
        offset_internal = c.place(4, 4);
        if version >= 414 {
            c.place(4, 4); // BlueprintReplicationCondition as uint32
        }
    } else {
        c.place(1, 1); // BlueprintReplicationCondition as a byte
        offset_internal = c.place(4, 4);
        if version < 503 {
            c.place(fname, fname_align); // RepNotifyFunc
        }
    }
    // PropertyLinkNext, NextRef, DestructorLinkNext, PostConstructLinkNext.
    c.place(0x20, 8);
    if version >= 503 {
        c.place(fname, 4); // RepNotifyFunc moved to the end in UE 5.3
    }
    (
        array_dim,
        element_size,
        flags,
        offset_internal,
        align_up(c.0, 8),
    )
}

/// Derives the layout and shape of one engine version from its parameters.
///
/// Every offset that follows from the parameters is ASSUMED from the `jmap`
/// type descriptions. A test reproduces the measured values (`UStruct` super,
/// children and child properties, `UFunction` func, `FField` next and name,
/// property element size and offset) for the builds that were measured.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flat derivation, easier to audit against jmap"
)]
pub fn derive(params: &Params) -> (EngineLayout, Shape) {
    let v = params.version;
    let fname = params.fname.size;
    let obj_size = params.uobject.size;
    let modern = v >= FIRST_FFIELD;
    let fname_align = if (411..422).contains(&v) { 8 } else { 4 };

    // UField and UStruct.
    let ufield_end = obj_size + 8;
    let mut c = Cursor(ufield_end);
    if v >= 422 {
        c.place(16, 8); // FStructBaseChain
    }
    let super_struct = c.place(8, 8);
    let children = c.place(8, 8);
    let child_properties = (v >= FIRST_FFIELD).then(|| c.place(8, 8));
    let properties_size = c.place(4, 4);
    let min_alignment = c.place(4, 4); // 2 + 2 (StructStateFlags) from 5.6
    c.place(0x10, 8); // Script
    c.place(0x20, 8); // PropertyLink, RefLink, DestructorLink, PostConstructLink
    c.place(0x10, 8); // ScriptObjectReferences
    if v >= FIRST_FFIELD {
        c.place(8, 8); // UnresolvedScriptProperties
        c.place(8, 8); // UnversionedSchema
    }
    let ustruct_size = align_up(c.0, 8);

    // UFunction: flags first, then NumParms ... Func at +0x28.
    let ufunction_flags = ustruct_size;
    let native_func = ufunction_flags + 0x28;
    let sizeof_ufunction = native_func + 8;

    // UClass.
    let mut c = Cursor(ustruct_size);
    if (408..414).contains(&v) {
        c.place(8, 4); // FFastIndexingClassTreeRegistrar
    }
    if (414..422).contains(&v) {
        c.place(16, 8); // FClassBaseChain
    }
    c.place(8, 8); // ClassConstructor
    c.place(8, 8); // ClassVTableHelperCtorCaller
    c.place(8, 8); // ClassAddReferencedObjects / CppClassStaticFunctions
    if (408..418).contains(&v) || v >= 500 {
        c.place(4, 4); // ClassUnique
    }
    if v >= 500 {
        c.place(4, 4); // FirstOwnedClassRep
        c.place(2, 1); // bCooked, bLayoutChanging
    } else if v >= 418 {
        c.place(4, 4); // ClassUnique:1, bCooked:1
    }
    if v >= 508 {
        c.place(2, 1);
        c.place(4, 4); // PropertiesStartOffset
    }
    c.place(4, 4); // ClassFlags
    let class_cast_flags = c.place(8, 8);
    c.place(8, 8); // ClassWithin
    if v < 500 {
        c.place(8, 8); // ClassGeneratedBy
    }
    if v == 421 {
        c.place(8, 8); // UberGraphFramePointerProperty
    }
    c.place(fname, fname_align); // ClassConfigName
    if (408..418).contains(&v) {
        c.place(1, 1); // bCooked
    }
    c.place(0x10, 8); // ClassReps
    c.place(0x10, 8); // NetFields
    if (425..500).contains(&v) {
        c.place(4, 4); // FirstOwnedClassRep
    }
    let class_default_object = c.place(8, 8);
    if v >= 424 {
        c.place(16, 8); // SparseClassData, SparseClassDataStruct
    }
    c.place(0x50, 8); // FuncMap
    if (411..418).contains(&v) {
        c.place(0xA0, 8); // ParentFuncMap, InterfaceFuncMap
    }
    if v >= 502 {
        c.place(8, 8); // FuncMapLock
    }
    if (418..503).contains(&v) {
        c.place(0x50, 8); // SuperFuncMap
        if v >= 421 {
            c.place(8, 8);
        }
    }
    if v >= 503 {
        c.place(0x58, 8); // AllFunctionsCache and its lock
    }
    c.place(0x10, 8); // Interfaces
    if v >= 503 {
        c.place(8, 8); // ReferenceSchema
    } else if v == 502 {
        c.place(32, 16);
    } else {
        c.place(0x10, 8); // ReferenceTokenStream
        if v >= 415 {
            c.place(0x28, 8); // ReferenceTokenStreamCritical
        }
    }
    c.place(0x10, 8); // NativeFunctionLookupTable
    if v >= 508 {
        c.place(8, 8); // Partials
    }
    let sizeof_uclass = align_up(c.0, 8);

    // UEnum.
    let enum_names = ufield_end + 0x10;
    let value_width: u32 = if v < 415 { 1 } else { 8 };
    let uenum = if params.enum_split {
        UEnumLayout::SplitArrays {
            names: enum_names,
            values: enum_names + 8,
            count: enum_names + 16,
        }
    } else {
        let value_offset = align_up(fname, value_width.min(8));
        let tuple_align = fname_align.max(value_width);
        UEnumLayout::PairArray {
            offset: enum_names,
            stride: align_up(value_offset + value_width, tuple_align),
            value_offset,
            value_width,
        }
    };
    let mut c = Cursor(enum_names);
    c.place(if params.enum_split { 0x18 } else { 0x10 }, 8);
    c.place(if v >= 505 { 1 } else { 4 }, 4); // CppForm
    if v >= 426 {
        c.place(if v >= 505 { 1 } else { 4 }, 4); // EnumFlags
    }
    if v >= 501 {
        c.place(fname, 4); // EnumPackage
    }
    if v >= 415 {
        c.place(8, 8); // EnumDisplayNameFn
    }
    let sizeof_uenum = align_up(c.0, 8);

    // Properties.
    let (ffield_layout, ffield_shape, field_class, field_class_shape, base_end) = if modern {
        let next = 0x10 + params.ffield_owner_size;
        let name = next + 8;
        let flags = align_up(name + fname, 4);
        let size = align_up(flags + 4, 8);
        // FFieldClass: name, [ClassFlags before Id from 5.7], Id, CastFlags,
        // [ClassFlags], SuperClass, DefaultObject, ConstructFn, counter.
        let name_off = if params.field_class_vtable { 8 } else { 0 };
        let mut fc = Cursor(name_off + fname);
        let early_flags = (v >= 507).then(|| fc.place(4, 4));
        let id = fc.place(8, 8);
        let cast = fc.place(8, 8);
        let class_flags = early_flags.unwrap_or_else(|| fc.place(4, 4));
        let super_class = fc.place(8, 8);
        let default_object = fc.place(8, 8);
        let construct_fn = fc.place(8, 8);
        fc.place(4, 4);
        (
            Some(FFieldLayout {
                class: 8,
                next,
                name,
                flags: Some(flags),
            }),
            Some(FFieldShape {
                owner: 0x10,
                owner_is_uobject: (params.ffield_owner_size > 8).then_some(0x18),
                size,
            }),
            Some(FFieldClassLayout {
                name: name_off,
                cast_flags: Some(cast),
            }),
            Some(FieldClassShape {
                id,
                class_flags,
                super_class,
                default_object,
                construct_fn,
                size: align_up(fc.0, 8),
            }),
            size,
        )
    } else {
        (None, None, None, None, ufield_end)
    };
    let (array_dim, element_size, property_flags, offset_internal, property_size) =
        property_members(v, base_end, fname);
    let system = match (ffield_layout, field_class) {
        (Some(ffield), Some(field_class)) => PropertySystem::Modern {
            ffield,
            field_class,
        },
        _ => PropertySystem::Legacy,
    };

    let layout = EngineLayout {
        pointer_size: 8,
        names: params.names.clone(),
        objects: params.objects.clone(),
        fname: params.fname.clone(),
        uobject: params.uobject.clone(),
        ufield: UFieldLayout { next: obj_size },
        ustruct: UStructLayout {
            super_struct,
            children,
            child_properties,
            properties_size,
            min_alignment,
            min_alignment_width: params.min_alignment_width,
        },
        uclass: UClassLayout {
            cast_flags: Some(class_cast_flags),
            default_object: Some(class_default_object),
        },
        ufunction: UFunctionLayout {
            flags: ufunction_flags,
            native_func: Some(native_func),
        },
        uenum,
        properties: PropertyLayout {
            system,
            array_dim,
            element_size,
            flags: property_flags,
            offset_internal,
            size: property_size,
            typed: typed_payloads(property_size, params.array_flags_first),
        },
    };
    let shape = Shape {
        version: v,
        sizeof_ufield: ufield_end,
        sizeof_ustruct: ustruct_size,
        sizeof_uclass,
        sizeof_ufunction,
        sizeof_uscriptstruct: ustruct_size + 0x10,
        sizeof_uenum,
        sizeof_upackage: obj_size + 0x80,
        ufunction_parm_block: if v >= 418 { 4 } else { 6 },
        object_array_prealloc: params.object_array_prealloc,
        object_array_size: params.object_array_size,
        item_flags: params.item_flags,
        item_cluster_root: params.item_cluster_root,
        process_event_index: params.process_event_index,
        vtable_slots: params.process_event_index + 0x20,
        vector_double: params.vector_double,
        multicast_inline_class: if v >= FIRST_POOL {
            "MulticastInlineDelegateProperty".to_owned()
        } else {
            "MulticastDelegateProperty".to_owned()
        },
        legacy_asset_names: v < 415,
        has_enum_and_set_properties: v >= 415,
        enum_cpp_type: ufield_end,
        ffield: ffield_shape,
        field_class: field_class_shape,
        name_pool_max_blocks: POOL_MAX_BLOCKS,
        name_array_max_chunks: NAME_ARRAY_CHUNKS,
        object_array_capacity: if params.objects.kind == ObjectArrayKind::Fixed {
            FIXED_OBJECT_CAPACITY
        } else {
            OBJECT_CAPACITY
        },
    };
    (layout, shape)
}

/// ProcessEvent's vtable index per era. 4.21 and 4.22 are MEASURED (0x40); the
/// rest is ASSUMED, since `jmap` does not describe vtables.
fn process_event_index(version: u32) -> u32 {
    match version {
        ..=422 => 0x40,
        423..=427 => 0x44,
        428..=502 => 0x4C,
        _ => 0x4D,
    }
}

/// The parameters of a stock build of `version` under `options`.
fn era_params(version: u32, options: BuildOptions) -> Params {
    let fname = if options.case_preserving_names {
        if version >= 501 {
            // jmap: DisplayIndex follows Number from UE 5.1.
            FNameLayout {
                size: 12,
                comparison_index: 0,
                display_index: Some(8),
                number: Some(4),
            }
        } else {
            FNameLayout {
                size: 12,
                comparison_index: 0,
                display_index: Some(4),
                number: Some(8),
            }
        }
    } else {
        FNameLayout {
            size: 8,
            comparison_index: 0,
            display_index: None,
            number: Some(4),
        }
    };
    let entry = options.pool_entry.unwrap_or(PoolEntryFormat {
        comparison_id: options.case_preserving_names,
        len_shift: if version >= 427 { 6 } else { 1 },
    });
    let (objects, prealloc, array_size, item_flags, item_cluster_root) = object_array_for(version);
    Params {
        version,
        uobject: standard_uobject(fname.size),
        names: names_for(version, entry),
        objects,
        object_array_prealloc: prealloc,
        object_array_size: array_size,
        item_flags,
        item_cluster_root,
        enum_split: version >= 507,
        min_alignment_width: if version >= 506 { 2 } else { 4 },
        vector_double: version >= 500,
        field_class_vtable: version >= 508,
        array_flags_first: version >= 503,
        ffield_owner_size: if version >= 503 { 8 } else { 16 },
        process_event_index: process_event_index(version),
        fname,
    }
}

/// Parameters of a case-preserving build of `version` (given as
/// `major * 100 + minor`), for the shuffled profiles to start from.
pub(crate) fn case_preserving_params(version: u32) -> Params {
    era_params(
        version,
        BuildOptions {
            case_preserving_names: true,
            pool_entry: None,
        },
    )
}

impl Profile {
    /// A stock build of `version`: no build options.
    ///
    /// # Errors
    ///
    /// [`ProfileError::UnsupportedVersion`] outside UE 4.11 to 5.8.
    pub fn stock(version: EngineVersion) -> Result<Self, ProfileError> {
        Self::with_options(version, BuildOptions::default())
    }

    /// A build of `version` with the given options.
    ///
    /// # Errors
    ///
    /// - [`ProfileError::UnsupportedVersion`] outside UE 4.11 to 5.8.
    /// - [`ProfileError::CasePreservingBeforeDisplayIndex`] for case-preserving
    ///   names before UE 4.22.
    /// - [`ProfileError::PoolEntryWithoutPool`] for a pool entry format before
    ///   UE 4.23.
    pub fn with_options(
        version: EngineVersion,
        options: BuildOptions,
    ) -> Result<Self, ProfileError> {
        let number = u32::from(version.major) * 100 + u32::from(version.minor);
        let supported = (411..=427).contains(&number) || (500..=508).contains(&number);
        if !supported {
            return Err(ProfileError::UnsupportedVersion(version));
        }
        if options.case_preserving_names && number < 422 {
            return Err(ProfileError::CasePreservingBeforeDisplayIndex(version));
        }
        if options.pool_entry.is_some() && number < FIRST_POOL {
            return Err(ProfileError::PoolEntryWithoutPool(version));
        }
        let params = era_params(number, options);
        let (layout, shape) = derive(&params);
        let suffix = if options.case_preserving_names {
            "-cp"
        } else {
            ""
        };
        let profile = Self {
            name: format!("ue{}.{}{suffix}", version.major, version.minor),
            identity: EngineIdentity {
                product_version: version.to_string(),
                // Invented: depends only on the version so a profile is stable.
                changelist: 1_000_000 + number * 100 + u32::from(version.patch),
                branch: "++Game+Live".to_owned(),
            },
            layout,
            shape,
        };
        debug_assert_eq!(profile.validate(), Ok(()));
        Ok(profile)
    }

    /// The version this profile describes.
    #[must_use]
    pub fn engine_version(&self) -> EngineVersion {
        let (major, minor, patch) = self.identity.version_parts();
        EngineVersion {
            major,
            minor,
            patch,
        }
    }

    fn measured(major: u16, minor: u16, patch: u16, case_preserving: bool) -> Self {
        let options = BuildOptions {
            case_preserving_names: case_preserving,
            pool_entry: None,
        };
        match Self::with_options(
            EngineVersion {
                major,
                minor,
                patch,
            },
            options,
        ) {
            Ok(profile) => profile,
            Err(error) => unreachable!("a measured build is always supported: {error}"),
        }
    }

    /// UE 4.21.1, `FName` of 8 bytes.
    ///
    /// MEASURED on a real executable: `FName` 8 bytes (comparison +0, number
    /// +4), `UObject` flags 8, index 0xC, class 0x10, name 0x18, outer 0x20,
    /// size 0x28, `UField::Next` 0x28, `UStruct` super 0x30, children 0x38,
    /// size 0x40 and alignment 0x44, `UFunction` flags 0x88 and func 0xB0,
    /// `UEnum::Names` 0x40, property array dim 0x30, element size 0x34, flags
    /// 0x38, offset 0x44 and `sizeof(UProperty)` 0x70, a chunked object array
    /// of 0x10000 items per chunk, `FUObjectItem` size 0x18 and a ProcessEvent
    /// vtable index of 0x40. Everything else is ASSUMED.
    #[must_use]
    pub fn ue4_21_measured() -> Self {
        Self::measured(4, 21, 1, false)
    }

    /// UE 4.22.3 with case-preserving names (12-byte `FName`).
    ///
    /// MEASURED: `FName` 12 bytes (comparison +0, display +4, number +8),
    /// `UObject` flags 8, index 0xC, class 0x10, name 0x18, outer 0x28, size
    /// 0x30, `UField::Next` 0x30, `UStruct` super 0x48, children 0x50, size
    /// 0x58 and alignment 0x5C, `UFunction` flags 0xA0 and func 0xC8,
    /// `UEnum::Names` 0x48, `UClass` cast flags 0xC0 and default object 0x108,
    /// property array dim 0x38, element size 0x3C, flags 0x40, offset 0x4C and
    /// `sizeof(UProperty)` 0x80, typed payloads at 0x80 (bool, struct, array
    /// inner and object class at 0x80, map key 0x80 and value 0x88, enum
    /// underlying 0x80 and enum 0x88, class meta 0x88), chunked object array,
    /// item size 0x18, ProcessEvent index 0x40. Everything else is ASSUMED.
    #[must_use]
    pub fn ue4_22_case_preserving_measured() -> Self {
        Self::measured(4, 22, 3, true)
    }

    /// UE 4.25.1 with case-preserving names (12-byte `FName`).
    ///
    /// MEASURED: `FName` 12 bytes, `UObject` offsets as 4.22, `UField::Next`
    /// 0x30, `UStruct` super 0x48, children 0x50 and child properties 0x58,
    /// `UFunction` func 0xE0, `FField` next 0x20 and name 0x28, property
    /// element size 0x3C and offset 0x4C, `FNamePool` with blocks at +0x10,
    /// stride 4, entry header at +4, characters at +6 and length `header >> 1`,
    /// chunked object array and item size 0x18. INFERRED: `UFunction` flags
    /// 0xB8, from a measured `ParmsSize` at 0xBE. Everything else is ASSUMED.
    #[must_use]
    pub fn ue4_25_case_preserving_measured() -> Self {
        Self::measured(4, 25, 1, true)
    }

    /// A UE5 build with case-preserving names, 5.3 as a stand-in for an
    /// unverified minor version.
    ///
    /// MEASURED: `Vector`, `Rotator` and `Quat` are double-based. Everything
    /// else is ASSUMED: a 12-byte `FName`, a 64-bit `FField` owner without the
    /// `bIsUObject` byte, `ArrayFlags` before the array inner property, and the
    /// rest of the 5.3 layout derived from `jmap`.
    #[must_use]
    pub fn ue5_case_preserving_assumed() -> Self {
        Self::measured(5, 3, 0, true)
    }

    /// Looks a profile up by command line name: `ue<major>.<minor>`,
    /// `ue<major>.<minor>-cp` for case-preserving names, `shuffled-<seed>`, or
    /// one of the measured variant names.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "ue4.21-measured" => return Some(Self::ue4_21_measured()),
            "ue4.22-cp-measured" => return Some(Self::ue4_22_case_preserving_measured()),
            "ue4.25-cp-measured" => return Some(Self::ue4_25_case_preserving_measured()),
            "ue5-cp-assumed" => return Some(Self::ue5_case_preserving_assumed()),
            _ => {}
        }
        if let Some(seed) = name.strip_prefix("shuffled-") {
            return seed.parse::<u64>().ok().map(Self::shuffled);
        }
        let (version, case_preserving) = match name.strip_suffix("-cp") {
            Some(version) => (version, true),
            None => (name, false),
        };
        let mut parts = version.strip_prefix("ue")?.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let options = BuildOptions {
            case_preserving_names: case_preserving,
            pool_entry: None,
        };
        Self::with_options(
            EngineVersion {
                major,
                minor,
                patch: 0,
            },
            options,
        )
        .ok()
    }

    /// `true` when properties are `FField` chains rather than `UProperty` objects.
    #[must_use]
    pub fn is_modern(&self) -> bool {
        matches!(self.layout.properties.system, PropertySystem::Modern { .. })
    }

    /// `true` when the name table is an `FNamePool`.
    #[must_use]
    pub fn uses_name_pool(&self) -> bool {
        matches!(self.layout.names, NameTableLayout::Pool(_))
    }
}
