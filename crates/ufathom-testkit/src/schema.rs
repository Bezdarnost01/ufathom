//! The reflection schema: which classes, structs and enums an image contains.
//!
//! The schema is expressed directly as `ufathom_model` definitions. The
//! writer materialises them into bytes and the same definitions are returned
//! as ground truth, so what is written and what is expected cannot drift.
//!
//! The member layouts of the engine classes are synthetic: they are not the
//! real members of `AActor` and friends, only a plausible arrangement that
//! covers every property type and has consistent sizes and offsets.

use std::collections::BTreeMap;

use ufathom_model::flags::{
    CPF_BLUEPRINT_VISIBLE, CPF_CONST_PARM, CPF_EDIT, CPF_NET, CPF_OUT_PARM, CPF_PARM,
    CPF_REFERENCE_PARM, CPF_RETURN_PARM, FUNC_BLUEPRINT_CALLABLE, FUNC_BLUEPRINT_EVENT,
    FUNC_BLUEPRINT_PURE, FUNC_CONST, FUNC_FINAL, FUNC_HAS_OUT_PARMS, FUNC_NATIVE, FUNC_NET,
    FUNC_NET_CLIENT, FUNC_NET_RELIABLE, FUNC_NET_SERVER, FUNC_STATIC,
};
use ufathom_model::{
    EnumDef, EnumValue, FieldDef, FunctionDef, PropertyType, StructDef, StructKind,
};

use crate::profile::{Profile, align_up};

/// `/Script/CoreUObject`.
pub const CORE_PACKAGE: &str = "/Script/CoreUObject";
/// `/Script/Engine`.
pub const ENGINE_PACKAGE: &str = "/Script/Engine";

/// `CPF_ZeroConstructor`.
pub(crate) const CPF_ZERO_CONSTRUCTOR: u64 = 0x200;
/// `CPF_IsPlainOldData`.
const CPF_IS_PLAIN_OLD_DATA: u64 = 0x4000_0000;
/// `CPF_NoDestructor`.
const CPF_NO_DESTRUCTOR: u64 = 0x10_0000_0000;
/// `CPF_HasGetValueTypeHash`.
const CPF_HAS_HASH: u64 = 0x8_0000_0000_0000;

/// `FUNC_Public`.
const FUNC_PUBLIC: u32 = 0x2_0000;
/// `FUNC_Event`.
const FUNC_EVENT: u32 = 0x800;
/// `FUNC_Delegate`.
const FUNC_DELEGATE: u32 = 0x10_0000;
/// `FUNC_MulticastDelegate`.
const FUNC_MULTICAST_DELEGATE: u32 = 0x1_0000;

/// Plain data members: visible in the editor and Blueprints.
const MEMBER: u64 = CPF_EDIT | CPF_BLUEPRINT_VISIBLE;
/// Members of trivially copyable types.
const POD: u64 = CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA | CPF_NO_DESTRUCTOR | CPF_HAS_HASH;

/// The property classes, with their parent class. The legacy system makes
/// each a `UClass`; the modern one makes each an `FFieldClass`.
pub const PROPERTY_CLASSES: &[(&str, &str)] = &[
    ("Property", "Field"),
    ("NumericProperty", "Property"),
    ("ByteProperty", "NumericProperty"),
    ("Int8Property", "NumericProperty"),
    ("Int16Property", "NumericProperty"),
    ("IntProperty", "NumericProperty"),
    ("Int64Property", "NumericProperty"),
    ("UInt16Property", "NumericProperty"),
    ("UInt32Property", "NumericProperty"),
    ("UInt64Property", "NumericProperty"),
    ("FloatProperty", "NumericProperty"),
    ("DoubleProperty", "NumericProperty"),
    ("BoolProperty", "Property"),
    ("ObjectPropertyBase", "Property"),
    ("ObjectProperty", "ObjectPropertyBase"),
    ("ClassProperty", "ObjectProperty"),
    ("WeakObjectProperty", "ObjectPropertyBase"),
    ("LazyObjectProperty", "ObjectPropertyBase"),
    ("SoftObjectProperty", "ObjectPropertyBase"),
    ("SoftClassProperty", "SoftObjectProperty"),
    ("InterfaceProperty", "Property"),
    ("NameProperty", "Property"),
    ("StrProperty", "Property"),
    ("TextProperty", "Property"),
    ("StructProperty", "Property"),
    ("ArrayProperty", "Property"),
    ("SetProperty", "Property"),
    ("MapProperty", "Property"),
    ("EnumProperty", "Property"),
    ("DelegateProperty", "Property"),
    ("MulticastDelegateProperty", "Property"),
    (
        "MulticastInlineDelegateProperty",
        "MulticastDelegateProperty",
    ),
    (
        "MulticastSparseDelegateProperty",
        "MulticastDelegateProperty",
    ),
    ("FieldPathProperty", "Property"),
];

/// Whether a property class exists in the profile's engine version.
///
/// The inline and sparse multicast classes appeared in UE 4.23 (before that
/// `MulticastDelegateProperty` was concrete), `FieldPathProperty` in 4.25, and
/// `EnumProperty` and `SetProperty` in 4.15.
#[must_use]
pub fn property_class_exists(name: &str, profile: &Profile) -> bool {
    let version = profile.shape.version;
    match name {
        "MulticastInlineDelegateProperty" | "MulticastSparseDelegateProperty" => version >= 423,
        "FieldPathProperty" => version >= 425,
        "EnumProperty" | "SetProperty" => profile.shape.has_enum_and_set_properties,
        _ => true,
    }
}

/// The name a property class has in the profile's engine version. Before UE
/// 4.15 soft pointers were called asset pointers.
#[must_use]
pub fn display_class_name(name: &str, profile: &Profile) -> String {
    if profile.shape.legacy_asset_names {
        match name {
            "SoftObjectProperty" => return "AssetObjectProperty".to_owned(),
            "SoftClassProperty" => return "AssetClassProperty".to_owned(),
            _ => {}
        }
    }
    name.to_owned()
}

/// The inverse of [`display_class_name`]: the name used in [`PROPERTY_CLASSES`].
#[must_use]
pub fn canonical_class_name(name: &str) -> &str {
    match name {
        "AssetObjectProperty" => "SoftObjectProperty",
        "AssetClassProperty" => "SoftClassProperty",
        other => other,
    }
}

/// The engine property class that stores a value of type `ty`.
#[must_use]
pub fn property_class_name(ty: &PropertyType, profile: &Profile) -> String {
    let name = match ty {
        PropertyType::Int8 => "Int8Property",
        PropertyType::Int16 => "Int16Property",
        PropertyType::Int32 => "IntProperty",
        PropertyType::Int64 => "Int64Property",
        PropertyType::UInt8 | PropertyType::Byte { .. } => "ByteProperty",
        PropertyType::UInt16 => "UInt16Property",
        PropertyType::UInt32 => "UInt32Property",
        PropertyType::UInt64 => "UInt64Property",
        PropertyType::Float => "FloatProperty",
        PropertyType::Double => "DoubleProperty",
        PropertyType::Bool { .. } => "BoolProperty",
        PropertyType::Name => "NameProperty",
        PropertyType::Str => "StrProperty",
        PropertyType::Text => "TextProperty",
        PropertyType::Object { .. } => "ObjectProperty",
        PropertyType::Class { .. } => "ClassProperty",
        PropertyType::WeakObject { .. } => "WeakObjectProperty",
        PropertyType::LazyObject { .. } => "LazyObjectProperty",
        PropertyType::SoftObject { .. } => {
            return display_class_name("SoftObjectProperty", profile);
        }
        PropertyType::SoftClass { .. } => {
            return display_class_name("SoftClassProperty", profile);
        }
        PropertyType::Interface { .. } => "InterfaceProperty",
        PropertyType::Struct { .. } => "StructProperty",
        PropertyType::Array { .. } => "ArrayProperty",
        PropertyType::Set { .. } => "SetProperty",
        PropertyType::Map { .. } => "MapProperty",
        PropertyType::Enum { .. } => "EnumProperty",
        PropertyType::Delegate { .. } => "DelegateProperty",
        PropertyType::MulticastInlineDelegate { .. } => {
            return profile.shape.multicast_inline_class.clone();
        }
        PropertyType::MulticastSparseDelegate { .. } => "MulticastSparseDelegateProperty",
        PropertyType::FieldPath => "FieldPathProperty",
        PropertyType::Optional { .. } => "OptionalProperty",
        PropertyType::Unknown { type_name } => return type_name.clone(),
    };
    name.to_owned()
}

/// Bytes a property object of class `ty` has beyond the common base. ASSUMED
/// from the `jmap` type descriptions; only used to size allocations.
#[must_use]
pub fn payload_size(ty: &PropertyType) -> u32 {
    match ty {
        // A `ByteProperty` always has room for its `Enum` pointer, null for a
        // plain byte.
        PropertyType::UInt8
        | PropertyType::Bool { .. }
        | PropertyType::Object { .. }
        | PropertyType::WeakObject { .. }
        | PropertyType::LazyObject { .. }
        | PropertyType::SoftObject { .. }
        | PropertyType::Interface { .. }
        | PropertyType::Struct { .. }
        | PropertyType::Byte { .. }
        | PropertyType::Delegate { .. }
        | PropertyType::MulticastInlineDelegate { .. }
        | PropertyType::MulticastSparseDelegate { .. }
        | PropertyType::FieldPath => 8,
        PropertyType::Class { .. }
        | PropertyType::SoftClass { .. }
        | PropertyType::Array { .. }
        | PropertyType::Enum { .. } => 0x10,
        PropertyType::Set { .. } => 0x28,
        PropertyType::Map { .. } => 0x38,
        _ => 0,
    }
}

/// Size and alignment of a value of type `ty` inside a struct.
///
/// ASSUMED from the `jmap` containers: `TArray` 0x10, `FString` 0x10, `FText`
/// 0x18, `TSet`/`TMap` 0x50, weak pointers 8, lazy pointers 0x18, soft
/// pointers 0x28, a single delegate `8 + sizeof(FName)`.
pub fn type_layout(
    ty: &PropertyType,
    profile: &Profile,
    structs: &BTreeMap<String, (u32, u32)>,
) -> (u32, u32) {
    let fname = profile.layout.fname.size;
    match ty {
        PropertyType::Int8 | PropertyType::UInt8 | PropertyType::Byte { .. } => (1, 1),
        PropertyType::Bool { field_size, .. } => (u32::from(*field_size), 1),
        PropertyType::Int16 | PropertyType::UInt16 => (2, 2),
        PropertyType::Int32 | PropertyType::UInt32 | PropertyType::Float => (4, 4),
        PropertyType::Enum { underlying, .. } => type_layout(underlying, profile, structs),
        PropertyType::Int64 | PropertyType::UInt64 | PropertyType::Double => (8, 8),
        PropertyType::Name => (fname, 4),
        PropertyType::Str | PropertyType::Array { .. } => (0x10, 8),
        PropertyType::Text => (0x18, 8),
        PropertyType::Object { .. } | PropertyType::Class { .. } | PropertyType::FieldPath => {
            (8, 8)
        }
        PropertyType::WeakObject { .. } => (8, 4),
        PropertyType::LazyObject { .. } => (0x18, 4),
        PropertyType::SoftObject { .. } | PropertyType::SoftClass { .. } => (0x28, 8),
        PropertyType::Interface { .. } => (0x10, 8),
        PropertyType::Struct { path } => structs.get(path).copied().unwrap_or((8, 8)),
        PropertyType::Set { .. } | PropertyType::Map { .. } => (0x50, 8),
        PropertyType::Delegate { .. } => (8 + fname, 4),
        PropertyType::MulticastInlineDelegate { .. } => (0x10, 8),
        PropertyType::MulticastSparseDelegate { .. } => (1, 1),
        PropertyType::Optional { inner } => {
            let (size, align) = type_layout(inner, profile, structs);
            (align_up(size + 1, align), align)
        }
        PropertyType::Unknown { .. } => (8, 8),
    }
}

/// Places members one after another at their natural alignment.
struct Layouter<'a> {
    profile: &'a Profile,
    structs: &'a BTreeMap<String, (u32, u32)>,
    cursor: u32,
    fields: Vec<FieldDef>,
    /// Open bit-field storage unit: its offset and the next free bit.
    bit_unit: Option<(u32, u8)>,
    max_align: u32,
}

impl<'a> Layouter<'a> {
    fn new(profile: &'a Profile, structs: &'a BTreeMap<String, (u32, u32)>, start: u32) -> Self {
        Self {
            profile,
            structs,
            cursor: start,
            fields: Vec::new(),
            bit_unit: None,
            max_align: 1,
        }
    }

    fn add_dim(&mut self, name: &str, ty: PropertyType, flags: u64, dim: u32) {
        self.add_aligned(name, ty, flags, dim, 1);
    }

    fn add_aligned(&mut self, name: &str, ty: PropertyType, flags: u64, dim: u32, min_align: u32) {
        let (size, align) = type_layout(&ty, self.profile, self.structs);
        let align = align.max(min_align);
        self.bit_unit = None;
        self.cursor = align_up(self.cursor, align);
        self.max_align = self.max_align.max(align);
        self.fields.push(FieldDef {
            name: name.to_owned(),
            offset: self.cursor,
            ty,
            element_size: size,
            array_dim: dim,
            flags,
        });
        self.cursor += size * dim;
    }

    fn add(&mut self, name: &str, ty: PropertyType, flags: u64) {
        self.add_dim(name, ty, flags, 1);
    }

    /// A bit-field `bool`, packed into the current byte while bits remain.
    fn add_bit(&mut self, name: &str, flags: u64) {
        let (offset, bit) = match self.bit_unit {
            Some((offset, bit)) if bit < 8 => (offset, bit),
            _ => {
                let offset = self.cursor;
                self.cursor += 1;
                (offset, 0)
            }
        };
        self.bit_unit = Some((offset, bit + 1));
        let mask = 1u8 << bit;
        self.fields.push(FieldDef {
            name: name.to_owned(),
            offset,
            ty: PropertyType::Bool {
                byte_offset: 0,
                byte_mask: mask,
                field_mask: mask,
                field_size: 1,
            },
            element_size: 1,
            array_dim: 1,
            flags,
        });
    }

    fn size(&self) -> u32 {
        align_up(self.cursor, self.max_align.max(4))
    }
}

fn path(package: &str, name: &str) -> String {
    format!("{package}.{name}")
}

fn object_of(package: &str, class: &str) -> PropertyType {
    PropertyType::Object {
        class: path(package, class),
    }
}

fn script_struct(name: &str) -> PropertyType {
    PropertyType::Struct {
        path: path(CORE_PACKAGE, name),
    }
}

fn enum_path(name: &str) -> String {
    path(ENGINE_PACKAGE, name)
}

fn param(name: &str, offset: u32, ty: PropertyType, size: u32, flags: u64) -> FieldDef {
    FieldDef {
        name: name.to_owned(),
        offset,
        ty,
        element_size: size,
        array_dim: 1,
        flags,
    }
}

/// A function builder: parameters are laid out in declaration order.
struct FunctionBuilder<'a> {
    profile: &'a Profile,
    structs: &'a BTreeMap<String, (u32, u32)>,
    class_path: String,
    name: String,
    flags: u32,
    cursor: u32,
    max_align: u32,
    params: Vec<FieldDef>,
}

impl<'a> FunctionBuilder<'a> {
    fn new(
        profile: &'a Profile,
        structs: &'a BTreeMap<String, (u32, u32)>,
        class_path: &str,
        name: &str,
        flags: u32,
    ) -> Self {
        Self {
            profile,
            structs,
            class_path: class_path.to_owned(),
            name: name.to_owned(),
            flags,
            cursor: 0,
            max_align: 1,
            params: Vec::new(),
        }
    }

    fn param(mut self, name: &str, ty: PropertyType, flags: u64) -> Self {
        let (size, align) = match &ty {
            // A native bool parameter occupies one byte and uses the full mask.
            PropertyType::Bool { .. } => (1, 1),
            other => type_layout(other, self.profile, self.structs),
        };
        self.cursor = align_up(self.cursor, align);
        self.max_align = self.max_align.max(align);
        self.params
            .push(param(name, self.cursor, ty, size, flags | CPF_PARM));
        self.cursor += size;
        self
    }

    /// A native `bool` parameter: byte 0, full mask.
    fn bool_param(self, name: &str, flags: u64) -> Self {
        self.param(
            name,
            PropertyType::Bool {
                byte_offset: 0,
                byte_mask: 0xFF,
                field_mask: 0xFF,
                field_size: 1,
            },
            flags | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA | CPF_NO_DESTRUCTOR,
        )
    }

    fn returns(self, name: &str, ty: PropertyType) -> Self {
        let is_bool = matches!(ty, PropertyType::Bool { .. });
        let flags = CPF_OUT_PARM | CPF_RETURN_PARM | CPF_ZERO_CONSTRUCTOR;
        if is_bool {
            self.bool_param(name, flags)
        } else {
            self.param(name, ty, flags | CPF_IS_PLAIN_OLD_DATA)
        }
    }

    fn build(self) -> FunctionDef {
        let has_out = self.params.iter().any(|p| p.flags & CPF_OUT_PARM != 0);
        let flags = if has_out {
            self.flags | FUNC_HAS_OUT_PARMS
        } else {
            self.flags
        };
        FunctionDef {
            path: format!("{}:{}", self.class_path, self.name),
            name: self.name,
            flags,
            params_size: align_up(self.cursor, self.max_align),
            params: self.params,
            native_rva: None,
        }
    }
}

/// The complete schema of an image.
#[derive(Debug, Clone)]
pub struct Schema {
    /// Every class and script struct, normalised (sorted by path).
    pub structs: Vec<StructDef>,
    /// Every enum, normalised.
    pub enums: Vec<EnumDef>,
}

fn script(
    profile: &Profile,
    structs: &mut BTreeMap<String, (u32, u32)>,
    name: &str,
    members: &[(&str, PropertyType)],
    align: u32,
) -> StructDef {
    let mut layouter = Layouter::new(profile, structs, 0);
    for (member, ty) in members {
        // The vectorised members of a Transform are each 16-byte aligned.
        let min_align = if align == 16 && name == "Transform" {
            16
        } else {
            1
        };
        layouter.add_aligned(member, ty.clone(), MEMBER | POD, 1, min_align);
    }
    layouter.max_align = layouter.max_align.max(align);
    let size = layouter.size();
    let fields = std::mem::take(&mut layouter.fields);
    let full = path(CORE_PACKAGE, name);
    structs.insert(full.clone(), (size, align));
    StructDef {
        path: full,
        name: name.to_owned(),
        package: CORE_PACKAGE.to_owned(),
        kind: StructKind::ScriptStruct,
        super_path: None,
        size,
        alignment: align,
        fields,
        functions: Vec::new(),
        flags: 0,
    }
}

fn class_def(
    package: &str,
    name: &str,
    super_path: Option<&str>,
    size: u32,
    fields: Vec<FieldDef>,
    functions: Vec<FunctionDef>,
) -> StructDef {
    StructDef {
        path: path(package, name),
        name: name.to_owned(),
        package: package.to_owned(),
        kind: StructKind::Class,
        super_path: super_path.map(str::to_owned),
        size,
        alignment: 8,
        fields,
        functions,
        flags: 0,
    }
}

fn enum_def(name: &str, max_name: &str, prefix_values: &[(&str, i64)]) -> EnumDef {
    let mut values: Vec<EnumValue> = prefix_values
        .iter()
        .map(|&(n, v)| EnumValue {
            name: n.to_owned(),
            value: v,
        })
        .collect();
    let max = prefix_values.iter().map(|&(_, v)| v).max().unwrap_or(0) + 1;
    // The engine appends `<Enum>_MAX` to every enum.
    values.push(EnumValue {
        name: max_name.to_owned(),
        value: max,
    });
    EnumDef {
        path: enum_path(name),
        name: name.to_owned(),
        package: ENGINE_PACKAGE.to_owned(),
        underlying_size: 1,
        values,
    }
}

/// Builds the schema for a profile.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flat table of the synthetic classes"
)]
pub fn build_schema(profile: &Profile) -> Schema {
    let shape = &profile.shape;
    let obj_size = profile.layout.uobject.size;
    let mut sizes: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut structs: Vec<StructDef> = Vec::new();

    // Meta-classes. Their properties are not reflected (the UObject members
    // are private C++ members), so they have no fields.
    let meta: [(&str, Option<&str>, u32); 8] = [
        ("Object", None, obj_size),
        ("Field", Some("Object"), shape.sizeof_ufield),
        ("Struct", Some("Field"), shape.sizeof_ustruct),
        ("Class", Some("Struct"), shape.sizeof_uclass),
        ("Function", Some("Struct"), shape.sizeof_ufunction),
        ("ScriptStruct", Some("Struct"), shape.sizeof_uscriptstruct),
        ("Enum", Some("Field"), shape.sizeof_uenum),
        ("Package", Some("Object"), shape.sizeof_upackage),
    ];
    for (name, parent, size) in meta {
        let parent_path = parent.map(|p| path(CORE_PACKAGE, p));
        structs.push(class_def(
            CORE_PACKAGE,
            name,
            parent_path.as_deref(),
            size,
            Vec::new(),
            Vec::new(),
        ));
    }
    if !profile.is_modern() {
        for &(name, parent) in PROPERTY_CLASSES {
            if !property_class_exists(name, profile) {
                continue;
            }
            // The legacy base class is `Property`; its size is the measured one.
            let size = profile.layout.properties.size
                + if name == "Property" || name.ends_with("NumericProperty") {
                    0
                } else {
                    8
                };
            let parent_path = path(CORE_PACKAGE, &display_class_name(parent, profile));
            structs.push(class_def(
                CORE_PACKAGE,
                &display_class_name(name, profile),
                Some(&parent_path),
                size,
                Vec::new(),
                Vec::new(),
            ));
        }
    }

    // Script structs.
    let vec_ty = if shape.vector_double {
        PropertyType::Double
    } else {
        PropertyType::Float
    };
    let vec_align = if shape.vector_double { 8 } else { 4 };
    structs.push(script(
        profile,
        &mut sizes,
        "Guid",
        &[
            ("A", PropertyType::Int32),
            ("B", PropertyType::Int32),
            ("C", PropertyType::Int32),
            ("D", PropertyType::Int32),
        ],
        4,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "Color",
        &[
            ("B", PropertyType::UInt8),
            ("G", PropertyType::UInt8),
            ("R", PropertyType::UInt8),
            ("A", PropertyType::UInt8),
        ],
        4,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "Vector",
        &[
            ("X", vec_ty.clone()),
            ("Y", vec_ty.clone()),
            ("Z", vec_ty.clone()),
        ],
        vec_align,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "Rotator",
        &[
            ("Pitch", vec_ty.clone()),
            ("Yaw", vec_ty.clone()),
            ("Roll", vec_ty.clone()),
        ],
        vec_align,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "Quat",
        &[
            ("X", vec_ty.clone()),
            ("Y", vec_ty.clone()),
            ("Z", vec_ty.clone()),
            ("W", vec_ty.clone()),
        ],
        16,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "Transform",
        &[
            ("Rotation", script_struct("Quat")),
            ("Translation", script_struct("Vector")),
            ("Scale3D", script_struct("Vector")),
        ],
        16,
    ));
    structs.push(script(
        profile,
        &mut sizes,
        "LinearColor",
        &[
            ("R", PropertyType::Float),
            ("G", PropertyType::Float),
            ("B", PropertyType::Float),
            ("A", PropertyType::Float),
        ],
        4,
    ));

    let enums = vec![
        enum_def(
            "ENetRole",
            "ROLE_MAX",
            &[
                ("ROLE_None", 0),
                ("ROLE_SimulatedProxy", 1),
                ("ROLE_AutonomousProxy", 2),
                ("ROLE_Authority", 3),
            ],
        ),
        enum_def(
            "EAutoPossessAI",
            "EAutoPossessAI_MAX",
            &[
                ("Disabled", 0),
                ("PlacedInWorld", 1),
                ("Spawned", 2),
                ("PlacedInWorldOrSpawned", 3),
            ],
        ),
    ];

    let object_path = path(CORE_PACKAGE, "Object");
    let actor_path = path(ENGINE_PACKAGE, "Actor");
    let pawn_path = path(ENGINE_PACKAGE, "Pawn");
    let controller_path = path(ENGINE_PACKAGE, "Controller");
    let engine_path = path(ENGINE_PACKAGE, "Engine");
    let actor_obj = || object_of(ENGINE_PACKAGE, "Actor");
    let signature = |class: &str, name: &str| format!("{}:{name}", path(ENGINE_PACKAGE, class));

    // Actor
    let mut a = Layouter::new(profile, &sizes, obj_size);
    a.add_bit("bHidden", MEMBER);
    a.add_bit("bCanBeDamaged", MEMBER);
    a.add_bit("bReplicateMovement", MEMBER | CPF_NET);
    a.add(
        "Role",
        PropertyType::Byte {
            path: enum_path("ENetRole"),
        },
        MEMBER | CPF_NET | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    a.add(
        "RemoteRole",
        PropertyType::Byte {
            path: enum_path("ENetRole"),
        },
        MEMBER | CPF_NET | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    if shape.has_enum_and_set_properties {
        a.add(
            "PreviousRole",
            PropertyType::Enum {
                path: enum_path("ENetRole"),
                underlying: Box::new(PropertyType::UInt8),
            },
            MEMBER | POD,
        );
    }
    a.add("NetTag", PropertyType::Int32, MEMBER | POD);
    a.add("NetUpdateFrequency", PropertyType::Float, MEMBER | POD);
    a.add("MinNetUpdateFrequency", PropertyType::Float, MEMBER | POD);
    a.add("ReplicatedCounter", PropertyType::Int64, MEMBER | POD);
    a.add("InitialLifeSpan", PropertyType::Double, MEMBER | POD);
    a.add(
        "Tags",
        PropertyType::Array {
            inner: Box::new(PropertyType::Name),
        },
        MEMBER,
    );
    a.add(
        "Owner",
        actor_obj(),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    a.add(
        "Instigator",
        object_of(ENGINE_PACKAGE, "Pawn"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    a.add(
        "ActorClassRef",
        PropertyType::Class {
            class: path(CORE_PACKAGE, "Class"),
            meta: actor_path.clone(),
        },
        MEMBER | CPF_ZERO_CONSTRUCTOR,
    );
    a.add(
        "SoftActor",
        PropertyType::SoftObject {
            class: actor_path.clone(),
        },
        MEMBER,
    );
    a.add(
        "SoftClassRef",
        PropertyType::SoftClass {
            meta: actor_path.clone(),
        },
        MEMBER,
    );
    a.add(
        "WeakOwner",
        PropertyType::WeakObject {
            class: actor_path.clone(),
        },
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    a.add(
        "LazyOwner",
        PropertyType::LazyObject {
            class: actor_path.clone(),
        },
        MEMBER,
    );
    a.add(
        "NamedActors",
        PropertyType::Map {
            key: Box::new(PropertyType::Name),
            value: Box::new(actor_obj()),
        },
        MEMBER,
    );
    if shape.has_enum_and_set_properties {
        a.add(
            "UniqueTags",
            PropertyType::Set {
                element: Box::new(PropertyType::Name),
            },
            MEMBER,
        );
    }
    a.add(
        "OnActorDestroyed",
        PropertyType::MulticastInlineDelegate {
            signature: signature("Actor", "OnActorDestroyed__DelegateSignature"),
        },
        MEMBER | 0x10000000,
    );
    a.add(
        "OnTakeHit",
        PropertyType::Delegate {
            signature: signature("Actor", "OnTakeHit__DelegateSignature"),
        },
        MEMBER,
    );
    a.add("ActorLabel", PropertyType::Name, MEMBER | POD);
    a.add("DebugName", PropertyType::Str, MEMBER);
    a.add("DisplayText", PropertyType::Text, MEMBER);
    a.add("ActorGuid", script_struct("Guid"), MEMBER | POD);
    a.add("ActorColor", script_struct("Color"), MEMBER | POD);
    a.add("Tint", script_struct("LinearColor"), MEMBER | POD);
    a.add("RelativeLocation", script_struct("Vector"), MEMBER | POD);
    a.add(
        "RelativeTransform",
        script_struct("Transform"),
        MEMBER | POD,
    );
    a.add_dim("SmallTable", PropertyType::Int32, MEMBER | POD, 3);
    a.add("ShortValue", PropertyType::Int16, MEMBER | POD);
    a.add("SignedByte", PropertyType::Int8, MEMBER | POD);
    a.add("WideValue", PropertyType::UInt16, MEMBER | POD);
    a.add("Flags", PropertyType::UInt32, MEMBER | POD);
    a.add("BigValue", PropertyType::UInt64, MEMBER | POD);
    a.add("TrailingByte", PropertyType::UInt8, MEMBER | POD);
    let actor_size = a.size();
    let actor_fields = std::mem::take(&mut a.fields);

    let native_public = FUNC_FINAL | FUNC_NATIVE | FUNC_PUBLIC | FUNC_BLUEPRINT_CALLABLE;
    let mut actor_functions = Vec::new();
    let begin_play = FunctionBuilder::new(
        profile,
        &sizes,
        &actor_path,
        "ReceiveBeginPlay",
        FUNC_EVENT | FUNC_PUBLIC | FUNC_BLUEPRINT_EVENT,
    );
    actor_functions.push(begin_play.build());
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "K2_DestroyActor",
            native_public,
        )
        .build(),
    );
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "GetActorLocation",
            native_public | FUNC_BLUEPRINT_PURE | FUNC_CONST,
        )
        .returns("ReturnValue", script_struct("Vector"))
        .build(),
    );
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "SetActorHiddenInGame",
            native_public,
        )
        .bool_param("bNewHidden", 0)
        .build(),
    );
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "K2_SetActorLocation",
            native_public,
        )
        .param(
            "NewLocation",
            script_struct("Vector"),
            CPF_CONST_PARM | CPF_OUT_PARM | CPF_REFERENCE_PARM | CPF_ZERO_CONSTRUCTOR,
        )
        .bool_param("bSweep", 0)
        .bool_param("bTeleport", 0)
        .returns(
            "ReturnValue",
            PropertyType::Bool {
                byte_offset: 0,
                byte_mask: 0xFF,
                field_mask: 0xFF,
                field_size: 1,
            },
        )
        .build(),
    );
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "OnActorDestroyed__DelegateSignature",
            FUNC_DELEGATE | FUNC_PUBLIC | FUNC_MULTICAST_DELEGATE,
        )
        .build(),
    );
    actor_functions.push(
        FunctionBuilder::new(
            profile,
            &sizes,
            &actor_path,
            "OnTakeHit__DelegateSignature",
            FUNC_DELEGATE | FUNC_PUBLIC,
        )
        .param(
            "HitActor",
            actor_obj(),
            CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
        )
        .build(),
    );
    structs.push(class_def(
        ENGINE_PACKAGE,
        "Actor",
        Some(&object_path),
        actor_size,
        actor_fields,
        actor_functions,
    ));

    // Pawn
    let mut p = Layouter::new(profile, &sizes, actor_size);
    p.add_bit("bUseControllerRotationYaw", MEMBER);
    p.add(
        "AutoPossessAI",
        PropertyType::Byte {
            path: enum_path("EAutoPossessAI"),
        },
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    p.add("BaseEyeHeight", PropertyType::Float, MEMBER | POD);
    p.add(
        "Controller",
        object_of(ENGINE_PACKAGE, "Controller"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    let pawn_size = p.size();
    let pawn_fields = std::mem::take(&mut p.fields);
    let pawn_functions = vec![
        FunctionBuilder::new(
            profile,
            &sizes,
            &pawn_path,
            "IsPawnControlled",
            native_public | FUNC_BLUEPRINT_PURE | FUNC_CONST,
        )
        .returns(
            "ReturnValue",
            PropertyType::Bool {
                byte_offset: 0,
                byte_mask: 0xFF,
                field_mask: 0xFF,
                field_size: 1,
            },
        )
        .build(),
    ];
    structs.push(class_def(
        ENGINE_PACKAGE,
        "Pawn",
        Some(&actor_path),
        pawn_size,
        pawn_fields,
        pawn_functions,
    ));

    // Controller
    let mut c = Layouter::new(profile, &sizes, actor_size);
    c.add(
        "Pawn",
        object_of(ENGINE_PACKAGE, "Pawn"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    c.add("ControlRotation", script_struct("Rotator"), MEMBER | POD);
    c.add_bit("bAttachToPawn", MEMBER);
    let controller_size = c.size();
    let controller_fields = std::mem::take(&mut c.fields);
    structs.push(class_def(
        ENGINE_PACKAGE,
        "Controller",
        Some(&actor_path),
        controller_size,
        controller_fields,
        Vec::new(),
    ));

    // PlayerController
    let mut pc = Layouter::new(profile, &sizes, controller_size);
    pc.add(
        "AcknowledgedPawn",
        object_of(ENGINE_PACKAGE, "Pawn"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    pc.add("NetPlayerIndex", PropertyType::UInt8, MEMBER | POD);
    pc.add(
        "CheatClass",
        PropertyType::Class {
            class: path(CORE_PACKAGE, "Class"),
            meta: path(ENGINE_PACKAGE, "Actor"),
        },
        MEMBER | CPF_ZERO_CONSTRUCTOR,
    );
    let pc_size = pc.size();
    let pc_fields = std::mem::take(&mut pc.fields);
    let pc_path = path(ENGINE_PACKAGE, "PlayerController");
    let pc_functions = vec![
        FunctionBuilder::new(
            profile,
            &sizes,
            &pc_path,
            "ClientRestart",
            FUNC_NET | FUNC_NET_RELIABLE | FUNC_NET_CLIENT | FUNC_EVENT | FUNC_PUBLIC | FUNC_NATIVE,
        )
        .param(
            "NewPawn",
            object_of(ENGINE_PACKAGE, "Pawn"),
            CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
        )
        .build(),
        FunctionBuilder::new(
            profile,
            &sizes,
            &pc_path,
            "ServerAcknowledgePossession",
            FUNC_NET | FUNC_NET_RELIABLE | FUNC_NET_SERVER | FUNC_EVENT | FUNC_PUBLIC | FUNC_NATIVE,
        )
        .param(
            "P",
            object_of(ENGINE_PACKAGE, "Pawn"),
            CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
        )
        .build(),
    ];
    structs.push(class_def(
        ENGINE_PACKAGE,
        "PlayerController",
        Some(&controller_path),
        pc_size,
        pc_fields,
        pc_functions,
    ));

    // Level
    let mut l = Layouter::new(profile, &sizes, obj_size);
    l.add(
        "OwningWorld",
        object_of(ENGINE_PACKAGE, "World"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    l.add(
        "Actors",
        PropertyType::Array {
            inner: Box::new(actor_obj()),
        },
        MEMBER,
    );
    l.add("LevelScale", PropertyType::Float, MEMBER | POD);
    let level_size = l.size();
    let level_fields = std::mem::take(&mut l.fields);
    structs.push(class_def(
        ENGINE_PACKAGE,
        "Level",
        Some(&object_path),
        level_size,
        level_fields,
        Vec::new(),
    ));

    // World
    let mut w = Layouter::new(profile, &sizes, obj_size);
    w.add(
        "PersistentLevel",
        object_of(ENGINE_PACKAGE, "Level"),
        MEMBER | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
    );
    w.add("NetDriverName", PropertyType::Name, MEMBER | POD);
    w.add("TimeSeconds", PropertyType::Float, MEMBER | POD);
    w.add_bit("bIsPaused", MEMBER);
    let world_size = w.size();
    let world_fields = std::mem::take(&mut w.fields);
    structs.push(class_def(
        ENGINE_PACKAGE,
        "World",
        Some(&object_path),
        world_size,
        world_fields,
        Vec::new(),
    ));

    // Engine and GameEngine
    let mut e = Layouter::new(profile, &sizes, obj_size);
    e.add("DefaultWorldName", PropertyType::Str, MEMBER);
    e.add("NearClipPlane", PropertyType::Float, MEMBER | POD);
    e.add("MaxFPS", PropertyType::Float, MEMBER | POD);
    e.add("GameViewportName", PropertyType::Name, MEMBER | POD);
    e.add_bit("bSmoothFrameRate", MEMBER);
    let engine_size = e.size();
    let engine_fields = std::mem::take(&mut e.fields);
    structs.push(class_def(
        ENGINE_PACKAGE,
        "Engine",
        Some(&object_path),
        engine_size,
        engine_fields,
        Vec::new(),
    ));
    let mut ge = Layouter::new(profile, &sizes, engine_size);
    ge.add("MaxIdleTime", PropertyType::Float, MEMBER | POD);
    ge.add("MaxDeltaTime", PropertyType::Float, MEMBER | POD);
    let ge_size = ge.size();
    let ge_fields = std::mem::take(&mut ge.fields);
    structs.push(class_def(
        ENGINE_PACKAGE,
        "GameEngine",
        Some(&engine_path),
        ge_size,
        ge_fields,
        Vec::new(),
    ));

    // Blueprint function libraries.
    let lib_size = obj_size;
    structs.push(class_def(
        ENGINE_PACKAGE,
        "BlueprintFunctionLibrary",
        Some(&object_path),
        lib_size,
        Vec::new(),
        Vec::new(),
    ));
    let kismet_path = path(ENGINE_PACKAGE, "KismetSystemLibrary");
    let static_public =
        FUNC_FINAL | FUNC_NATIVE | FUNC_STATIC | FUNC_PUBLIC | FUNC_BLUEPRINT_CALLABLE;
    let kismet_functions = vec![
        FunctionBuilder::new(
            profile,
            &sizes,
            &kismet_path,
            "GetGameName",
            static_public | FUNC_BLUEPRINT_PURE,
        )
        .returns("ReturnValue", PropertyType::Str)
        .build(),
        FunctionBuilder::new(profile, &sizes, &kismet_path, "PrintString", static_public)
            .param(
                "WorldContextObject",
                PropertyType::Object {
                    class: object_path.clone(),
                },
                CPF_CONST_PARM | CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
            )
            .param("InString", PropertyType::Str, 0)
            .bool_param("bPrintToScreen", 0)
            .bool_param("bPrintToLog", 0)
            .build(),
        FunctionBuilder::new(
            profile,
            &sizes,
            &kismet_path,
            "IsValid",
            static_public | FUNC_BLUEPRINT_PURE,
        )
        .param(
            "Object",
            PropertyType::Object {
                class: object_path.clone(),
            },
            CPF_ZERO_CONSTRUCTOR | CPF_IS_PLAIN_OLD_DATA,
        )
        .returns(
            "ReturnValue",
            PropertyType::Bool {
                byte_offset: 0,
                byte_mask: 0xFF,
                field_mask: 0xFF,
                field_size: 1,
            },
        )
        .build(),
    ];
    structs.push(class_def(
        ENGINE_PACKAGE,
        "KismetSystemLibrary",
        Some(&path(ENGINE_PACKAGE, "BlueprintFunctionLibrary")),
        lib_size,
        Vec::new(),
        kismet_functions,
    ));

    Schema { structs, enums }
}

impl Schema {
    /// The schema in the canonical order a normalised dump uses: structs and
    /// enums by path, fields by `(offset, name)`, functions by path and
    /// enumerators by `(value, name)`. Function parameters keep their
    /// declared order.
    #[must_use]
    pub fn normalized(&self) -> (Vec<StructDef>, Vec<EnumDef>) {
        let mut structs = self.structs.clone();
        for def in &mut structs {
            def.fields.sort_by(|a, b| {
                a.offset
                    .cmp(&b.offset)
                    .then_with(|| a.name.cmp(&b.name))
                    .then_with(|| a.cmp(b))
            });
            def.functions.sort();
        }
        structs.sort();
        let mut enums = self.enums.clone();
        for def in &mut enums {
            def.values
                .sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
        }
        enums.sort();
        (structs, enums)
    }
}
