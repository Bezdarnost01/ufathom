//! Dumps used by the tests: a hand-written one that covers every property type
//! and every degradation case, a small one for the behaviour test, and dumps
//! built from the synthetic engine images of `ufathom-testkit`.

use ufathom_model::flags::{CPF_CONST_PARM, CPF_OUT_PARM, CPF_PARM, CPF_RETURN_PARM};
use ufathom_model::{
    Dump, EngineLayout, EnumDef, EnumValue, FieldDef, FunctionDef, PropertyType, StructDef,
    StructKind,
};
use ufathom_testkit::{FixtureBuilder, Profile};

use super::make_dump;

pub const CORE: &str = "/Script/CoreUObject";
pub const ENGINE: &str = "/Script/Engine";
pub const MODS: &str = "/Game/Mods";

pub fn path(package: &str, name: &str) -> String {
    format!("{package}.{name}")
}

pub fn field(name: &str, offset: u32, ty: PropertyType, size: u32) -> FieldDef {
    FieldDef {
        name: name.to_owned(),
        offset,
        ty,
        element_size: size,
        array_dim: 1,
        flags: 0,
    }
}

pub fn array_field(name: &str, offset: u32, ty: PropertyType, size: u32, dim: u32) -> FieldDef {
    FieldDef {
        array_dim: dim,
        ..field(name, offset, ty, size)
    }
}

/// A bit-field `bool`: one bit of a one-byte unit.
pub fn bit(name: &str, offset: u32, byte_offset: u8, mask: u8) -> FieldDef {
    field(
        name,
        offset,
        PropertyType::Bool {
            byte_offset,
            byte_mask: mask,
            field_mask: mask,
            field_size: 1,
        },
        1,
    )
}

/// A native `bool`: the whole byte.
pub fn native_bool(name: &str, offset: u32) -> FieldDef {
    field(
        name,
        offset,
        PropertyType::Bool {
            byte_offset: 0,
            byte_mask: 0xFF,
            field_mask: 0xFF,
            field_size: 1,
        },
        1,
    )
}

pub fn structure(
    package: &str,
    name: &str,
    kind: StructKind,
    super_path: Option<&str>,
    size: u32,
    alignment: u32,
    fields: Vec<FieldDef>,
    functions: Vec<FunctionDef>,
) -> StructDef {
    StructDef {
        path: path(package, name),
        name: name.to_owned(),
        package: package.to_owned(),
        kind,
        super_path: super_path.map(str::to_owned),
        size,
        alignment,
        fields,
        functions,
        flags: 0,
    }
}

pub fn class(
    package: &str,
    name: &str,
    super_path: Option<&str>,
    size: u32,
    fields: Vec<FieldDef>,
    functions: Vec<FunctionDef>,
) -> StructDef {
    structure(
        package,
        name,
        StructKind::Class,
        super_path,
        size,
        8,
        fields,
        functions,
    )
}

pub fn script_struct(
    package: &str,
    name: &str,
    size: u32,
    alignment: u32,
    fields: Vec<FieldDef>,
) -> StructDef {
    structure(
        package,
        name,
        StructKind::ScriptStruct,
        None,
        size,
        alignment,
        fields,
        Vec::new(),
    )
}

pub fn param(name: &str, offset: u32, ty: PropertyType, size: u32, flags: u64) -> FieldDef {
    field(name, offset, ty, size).with_flags(flags | CPF_PARM)
}

trait WithFlags {
    fn with_flags(self, flags: u64) -> Self;
}

impl WithFlags for FieldDef {
    fn with_flags(mut self, flags: u64) -> Self {
        self.flags = flags;
        self
    }
}

pub fn function(owner: &str, name: &str, params_size: u32, params: Vec<FieldDef>) -> FunctionDef {
    FunctionDef {
        path: format!("{owner}:{name}"),
        name: name.to_owned(),
        flags: 0,
        params_size,
        params,
        native_rva: None,
    }
}

pub fn enum_def(
    package: &str,
    name: &str,
    underlying_size: u32,
    values: &[(&str, i64)],
) -> EnumDef {
    EnumDef {
        path: path(package, name),
        name: name.to_owned(),
        package: package.to_owned(),
        underlying_size,
        values: values
            .iter()
            .map(|&(n, v)| EnumValue {
                name: n.to_owned(),
                value: v,
            })
            .collect(),
    }
}

/// Places fields one after another with natural alignment.
struct Cursor {
    offset: u32,
    fields: Vec<FieldDef>,
}

impl Cursor {
    fn new(start: u32) -> Self {
        Self {
            offset: start,
            fields: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, ty: PropertyType, size: u32, align: u32) -> &mut Self {
        self.add_array(name, ty, size, align, 1)
    }

    fn add_array(
        &mut self,
        name: &str,
        ty: PropertyType,
        size: u32,
        align: u32,
        dim: u32,
    ) -> &mut Self {
        self.offset = self.offset.div_ceil(align) * align;
        self.fields
            .push(array_field(name, self.offset, ty, size, dim));
        self.offset += size * dim;
        self
    }

    fn size(&self) -> u32 {
        self.offset.div_ceil(8) * 8
    }
}

fn boxed(ty: PropertyType) -> Box<PropertyType> {
    Box::new(ty)
}

/// A class with one field of every `PropertyType`, for a 12-byte `FName`.
fn everything() -> StructDef {
    let obj = |c: &str| PropertyType::Object {
        class: path(ENGINE, c),
    };
    let mut c = Cursor::new(0x30);
    c.add("Int8Value", PropertyType::Int8, 1, 1)
        .add("Int16Value", PropertyType::Int16, 2, 2)
        .add("Int32Value", PropertyType::Int32, 4, 4)
        .add("Int64Value", PropertyType::Int64, 8, 8)
        .add("UInt8Value", PropertyType::UInt8, 1, 1)
        .add("UInt16Value", PropertyType::UInt16, 2, 2)
        .add("UInt32Value", PropertyType::UInt32, 4, 4)
        .add("UInt64Value", PropertyType::UInt64, 8, 8)
        .add("FloatValue", PropertyType::Float, 4, 4)
        .add("DoubleValue", PropertyType::Double, 8, 8)
        .add("NativeFlag", native_bool_type(), 1, 1)
        .add("NameValue", PropertyType::Name, 12, 4)
        .add("StringValue", PropertyType::Str, 16, 8)
        .add("TextValue", PropertyType::Text, 0x18, 8)
        .add("ActorRef", obj("Actor"), 8, 8)
        .add(
            "MissingRef",
            PropertyType::Object {
                class: "/Script/Gone.Gone".into(),
            },
            8,
            8,
        )
        .add(
            "ClassRef",
            PropertyType::Class {
                class: path(CORE, "Class"),
                meta: path(ENGINE, "Actor"),
            },
            8,
            8,
        )
        .add(
            "WeakRef",
            PropertyType::WeakObject {
                class: path(ENGINE, "Actor"),
            },
            8,
            4,
        )
        .add(
            "LazyRef",
            PropertyType::LazyObject {
                class: path(ENGINE, "Actor"),
            },
            28,
            4,
        )
        .add(
            "SoftRef",
            PropertyType::SoftObject {
                class: path(ENGINE, "Actor"),
            },
            48,
            8,
        )
        .add(
            "SoftClassRef",
            PropertyType::SoftClass {
                meta: path(ENGINE, "Actor"),
            },
            48,
            8,
        )
        .add(
            "InterfaceRef",
            PropertyType::Interface {
                class: path(ENGINE, "Actor"),
            },
            16,
            8,
        )
        .add(
            "Position",
            PropertyType::Struct {
                path: path(CORE, "Vector"),
            },
            12,
            4,
        )
        .add(
            "Id",
            PropertyType::Struct {
                path: path(CORE, "Guid"),
            },
            16,
            4,
        )
        .add(
            "Pose",
            PropertyType::Struct {
                path: path(CORE, "Transform"),
            },
            0x20,
            16,
        )
        .add(
            "Numbers",
            PropertyType::Array {
                inner: boxed(PropertyType::Int32),
            },
            16,
            8,
        )
        .add(
            "Actors",
            PropertyType::Array {
                inner: boxed(obj("Actor")),
            },
            16,
            8,
        )
        .add(
            "Unresolved",
            PropertyType::Array {
                inner: boxed(PropertyType::Unknown {
                    type_name: "FooProperty".into(),
                }),
            },
            16,
            8,
        )
        .add(
            "Tags",
            PropertyType::Set {
                element: boxed(PropertyType::Name),
            },
            0x50,
            8,
        )
        .add(
            "Table",
            PropertyType::Map {
                key: boxed(PropertyType::Name),
                value: boxed(PropertyType::Struct {
                    path: path(CORE, "Vector"),
                }),
            },
            0x50,
            8,
        )
        .add(
            "Role",
            PropertyType::Enum {
                path: path(CORE, "ENetRole"),
                underlying: boxed(PropertyType::UInt8),
            },
            1,
            1,
        )
        .add(
            "RoleByte",
            PropertyType::Byte {
                path: path(CORE, "ENetRole"),
            },
            1,
            1,
        )
        .add(
            "Signed",
            PropertyType::Enum {
                path: path(CORE, "ESigned"),
                underlying: boxed(PropertyType::Int32),
            },
            4,
            4,
        )
        .add(
            "GoneEnum",
            PropertyType::Enum {
                path: "/Script/Gone.EGone".into(),
                underlying: boxed(PropertyType::UInt16),
            },
            2,
            2,
        )
        .add(
            "GoneByte",
            PropertyType::Byte {
                path: "/Script/Gone.EGone".into(),
            },
            1,
            1,
        )
        .add(
            "OnDone",
            PropertyType::Delegate {
                signature: path(ENGINE, "Everything:OnDone__Sig"),
            },
            20,
            4,
        )
        .add(
            "OnMany",
            PropertyType::MulticastInlineDelegate {
                signature: path(ENGINE, "Everything:OnMany__Sig"),
            },
            16,
            8,
        )
        .add(
            "OnSparse",
            PropertyType::MulticastSparseDelegate {
                signature: path(ENGINE, "Everything:OnSparse__Sig"),
            },
            1,
            1,
        )
        .add("PathValue", PropertyType::FieldPath, 32, 8)
        .add(
            "MaybeInt",
            PropertyType::Optional {
                inner: boxed(PropertyType::Int32),
            },
            8,
            4,
        )
        .add(
            "Mystery",
            PropertyType::Unknown {
                type_name: "MysteryProperty".into(),
            },
            16,
            8,
        )
        .add_array("Grid", PropertyType::Int32, 4, 4, 4)
        .add_array(
            "Corners",
            PropertyType::Struct {
                path: path(CORE, "Vector"),
            },
            12,
            4,
            2,
        );
    let mut fields = std::mem::take(&mut c.fields);
    fields.push(bit("bFirst", c.offset, 0, 0x01));
    fields.push(bit("bSecond", c.offset, 0, 0x02));
    let size = (c.offset + 1).div_ceil(16) * 16;
    class(
        ENGINE,
        "Everything",
        Some(&path(CORE, "Object")),
        size,
        fields,
        Vec::new(),
    )
}

fn native_bool_type() -> PropertyType {
    PropertyType::Bool {
        byte_offset: 0,
        byte_mask: 0xFF,
        field_mask: 0xFF,
        field_size: 1,
    }
}

fn actor_functions() -> Vec<FunctionDef> {
    let owner = path(ENGINE, "Actor");
    let ret = |ty: PropertyType, size: u32| param("ReturnValue", 0, ty, size, CPF_RETURN_PARM);
    let vector = PropertyType::Struct {
        path: path(CORE, "Vector"),
    };
    vec![
        function(
            &owner,
            "K2_DestroyActor",
            1,
            vec![ret(native_bool_type(), 1)],
        ),
        function(
            &owner,
            "SetHealth",
            0x10,
            vec![
                param("NewHealth", 0, PropertyType::Float, 4, 0),
                param("bForce", 4, native_bool_type(), 1, 0),
                param("OldHealth", 8, PropertyType::Float, 4, CPF_OUT_PARM),
                param("ReturnValue", 12, native_bool_type(), 1, CPF_RETURN_PARM),
            ],
        ),
        function(
            &owner,
            "GetLocation",
            12,
            vec![param("ReturnValue", 0, vector.clone(), 12, CPF_RETURN_PARM)],
        ),
        function(
            &owner,
            "TeleportTo",
            0x20,
            vec![
                param("Target", 0, vector, 12, CPF_CONST_PARM | CPF_OUT_PARM),
                // A bit-field bool parameter, and names the wrapper itself uses.
                param("exec", 12, PropertyType::Int32, 4, 0),
                param("this", 16, PropertyType::Int32, 4, 0),
                param("params", 20, PropertyType::Int32, 4, 0),
                {
                    let mut b = bit("bSweep", 24, 0, 0x01);
                    b.flags = CPF_PARM;
                    b
                },
                param("Hit", 28, PropertyType::Int32, 4, CPF_OUT_PARM),
            ],
        ),
        // Parameters that overlap cannot be placed: no wrapper.
        function(
            &owner,
            "Overlapping",
            8,
            vec![
                param("A", 0, PropertyType::UInt64, 8, 0),
                param("B", 4, PropertyType::UInt32, 4, 0),
            ],
        ),
        function(
            &owner,
            "TakesOptional",
            8,
            vec![param(
                "Value",
                0,
                PropertyType::Optional {
                    inner: boxed(PropertyType::Int32),
                },
                8,
                0,
            )],
        ),
        function(
            &owner,
            "GetLabel",
            16,
            vec![param(
                "ReturnValue",
                0,
                PropertyType::Str,
                16,
                CPF_RETURN_PARM,
            )],
        ),
        function(&owner, "Ping", 0, Vec::new()),
    ]
}

/// Classes that each trigger one degradation or naming rule.
fn tricky() -> Vec<StructDef> {
    let object = path(CORE, "Object");
    let u32t = || PropertyType::UInt32;
    let u64t = || PropertyType::UInt64;
    let u8t = || PropertyType::UInt8;
    let mut out = vec![
        // Overlap, and a field past the end.
        class(
            ENGINE,
            "Overlapper",
            Some(&object),
            0x40,
            vec![
                field("Big", 0x30, u64t(), 8),
                field("Inner", 0x34, u32t(), 4),
                field("After", 0x38, u32t(), 4),
                field("Past", 0x40, u32t(), 4),
            ],
            Vec::new(),
        ),
        // A u64 at a 4 byte offset.
        script_struct(
            ENGINE,
            "Misaligned",
            0x10,
            8,
            vec![
                field("A", 0, u8t(), 1),
                field("B", 4, u64t(), 8),
                field("C", 12, u32t(), 4),
            ],
        ),
        // Declared alignment below the alignment of a field type: the field is
        // aligned, so the generated type is raised to 8.
        script_struct(
            ENGINE,
            "LowAlign",
            16,
            4,
            vec![field("Wide", 0, u64t(), 8), field("N", 8, u32t(), 4)],
        ),
        // Size 0x29 is not a multiple of 8, 4 or 2.
        script_struct(
            ENGINE,
            "OddSize",
            0x29,
            8,
            vec![
                field("A", 0, u8t(), 1),
                field("B", 4, u32t(), 4),
                field("C", 0x28, u8t(), 1),
            ],
        ),
        // Size 0x2C is a multiple of 4 only.
        script_struct(
            ENGINE,
            "QuarterSize",
            0x2C,
            8,
            vec![field("A", 0, u32t(), 4), field("Q", 0x20, u64t(), 8)],
        ),
        // A declared alignment that is not a power of two counts as 1, then the
        // aligned field raises it to 4.
        script_struct(ENGINE, "WeirdAlign", 12, 12, vec![field("A", 0, u32t(), 4)]),
        script_struct(ENGINE, "EmptyStruct", 0, 1, Vec::new()),
        script_struct(ENGINE, "EmptyAligned", 0, 8, Vec::new()),
        // The parent ends after the first field of the child (tail padding reuse).
        class(
            ENGINE,
            "ReuseBase",
            None,
            0x10,
            vec![field("Id", 0, u32t(), 4), field("Flag", 8, u8t(), 1)],
            Vec::new(),
        ),
        class(
            ENGINE,
            "ReuseChild",
            Some(&path(ENGINE, "ReuseBase")),
            0x18,
            vec![
                field("Extra", 0x0C, u32t(), 4),
                field("Tail", 0x10, u64t(), 8),
            ],
            Vec::new(),
        ),
        // An empty base.
        class(ENGINE, "EmptyBase", None, 0, Vec::new(), Vec::new()),
        class(
            ENGINE,
            "FromEmpty",
            Some(&path(ENGINE, "EmptyBase")),
            8,
            vec![field("Value", 0, u64t(), 8)],
            Vec::new(),
        ),
        // Bit-field booleans sharing a byte, one in a second byte, one impossible.
        class(
            ENGINE,
            "BitShare",
            Some(&object),
            0x38,
            vec![
                bit("bFirst", 0x30, 0, 0x01),
                bit("bSecond", 0x30, 0, 0x02),
                bit("bThird", 0x30, 0, 0x80),
                bit("bNext", 0x31, 0, 0x01),
                bit("bBad", 0x32, 5, 0x01),
                native_bool("bPlain", 0x33),
            ],
            Vec::new(),
        ),
        // A parent that is not in the dump.
        class(
            ENGINE,
            "Orphan",
            Some("/Script/Missing.Thing"),
            0x20,
            vec![field("V", 0x10, u32t(), 4)],
            Vec::new(),
        ),
        // Two types with the same name in one package.
        class(
            ENGINE,
            "Dup",
            Some(&object),
            0x38,
            vec![field("A", 0x30, u32t(), 4)],
            Vec::new(),
        ),
        {
            let mut d = class(
                ENGINE,
                "Dup",
                Some(&object),
                0x38,
                vec![field("B", 0x30, u32t(), 4)],
                Vec::new(),
            );
            d.path = "/Script/Engine.Outer.Dup".to_owned();
            d
        },
        // By-value members that refer to each other.
        script_struct(
            ENGINE,
            "CycleA",
            8,
            4,
            vec![field(
                "B",
                0,
                PropertyType::Struct {
                    path: path(ENGINE, "CycleB"),
                },
                8,
            )],
        ),
        script_struct(
            ENGINE,
            "CycleB",
            8,
            4,
            vec![field(
                "A",
                0,
                PropertyType::Struct {
                    path: path(ENGINE, "CycleA"),
                },
                8,
            )],
        ),
        // A struct whose size in the dump disagrees with its type, and a missing struct.
        class(
            ENGINE,
            "SizeMismatch",
            Some(&object),
            0x50,
            vec![
                field(
                    "V",
                    0x30,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    16,
                ),
                field(
                    "Gone",
                    0x40,
                    PropertyType::Struct {
                        path: "/Script/Gone.FGone".into(),
                    },
                    8,
                ),
            ],
            Vec::new(),
        ),
        // Names that need sanitizing and names that collide.
        {
            let names = [
                "type",
                "Self",
                "match",
                "Weird Name!",
                "\u{540d}\u{524d}",
                "Foo",
                "foo",
                "9lives",
                "_",
                "crate",
                "self",
            ];
            let mut c = Cursor::new(0x30);
            for n in names {
                c.add(n, PropertyType::UInt32, 4, 4);
            }
            let size = c.size();
            class(ENGINE, "Type", Some(&object), size, c.fields, Vec::new())
        },
    ];
    out.extend(regressions());
    out.sort();
    out
}

/// Names that are keywords once turned into identifiers, and packages whose
/// module names would collide with special modules.
fn regressions() -> Vec<StructDef> {
    let object = path(CORE, "Object");
    let owner = path(ENGINE, "Keywords");
    let mut c = Cursor::new(0x38);
    c.add("Plain", PropertyType::UInt32, 4, 4);
    let mut fields = vec![
        // Bit-field booleans named like keywords: the setter is `set_loop`,
        // the getter `r#loop`.
        bit("Loop", 0x30, 0, 0x01),
        bit("Type", 0x30, 0, 0x02),
        bit("Self", 0x30, 0, 0x04),
        bit("bLoop", 0x30, 0, 0x08),
        native_bool("Match", 0x31),
    ];
    fields.extend(c.fields);
    let functions = vec![
        // Same names as the bits, and parameters named like keywords.
        function(
            &owner,
            "Loop",
            8,
            vec![
                param("type", 0, PropertyType::Int32, 4, 0),
                param("self", 4, PropertyType::Int32, 4, 0),
            ],
        ),
        function(
            &owner,
            "Type",
            1,
            vec![{
                let mut b = bit("Move", 0, 0, 0x01);
                b.flags = CPF_PARM;
                b
            }],
        ),
    ];
    vec![
        class(ENGINE, "Keywords", Some(&object), 0x40, fields, functions),
        // Packages named like special modules.
        class(
            "/Script/Main",
            "Entry",
            Some(&object),
            0x38,
            vec![field("Value", 0x30, PropertyType::UInt32, 4)],
            Vec::new(),
        ),
        class(
            "/Script/Std",
            "Entry",
            Some(&object),
            0x38,
            vec![field("Value", 0x30, PropertyType::UInt32, 4)],
            Vec::new(),
        ),
        class(
            "/Script/Core",
            "Entry",
            Some(&object),
            0x38,
            vec![field("Value", 0x30, PropertyType::UInt32, 4)],
            Vec::new(),
        ),
    ]
}

/// The hand-written dump behind the golden files.
pub fn golden_dump() -> Dump {
    let layout = Profile::ue4_25_case_preserving_measured().layout;
    let object = path(CORE, "Object");
    let actor = path(ENGINE, "Actor");
    let mut structs = vec![
        class(CORE, "Object", None, 0x30, Vec::new(), Vec::new()),
        class(CORE, "Field", Some(&object), 0x38, Vec::new(), Vec::new()),
        class(
            CORE,
            "Struct",
            Some(&path(CORE, "Field")),
            0x70,
            Vec::new(),
            Vec::new(),
        ),
        class(
            CORE,
            "Class",
            Some(&path(CORE, "Struct")),
            0x130,
            Vec::new(),
            Vec::new(),
        ),
        class(
            CORE,
            "Function",
            Some(&path(CORE, "Struct")),
            0xF0,
            Vec::new(),
            Vec::new(),
        ),
        script_struct(
            CORE,
            "Vector",
            12,
            4,
            vec![
                field("X", 0, PropertyType::Float, 4),
                field("Y", 4, PropertyType::Float, 4),
                field("Z", 8, PropertyType::Float, 4),
            ],
        ),
        script_struct(
            CORE,
            "Guid",
            16,
            4,
            vec![
                field("A", 0, PropertyType::UInt32, 4),
                field("B", 4, PropertyType::UInt32, 4),
                field("C", 8, PropertyType::UInt32, 4),
                field("D", 12, PropertyType::UInt32, 4),
            ],
        ),
        script_struct(
            CORE,
            "Transform",
            0x20,
            16,
            vec![
                field(
                    "Scale3D",
                    0,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    12,
                ),
                field(
                    "Translation",
                    0x10,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    12,
                ),
            ],
        ),
        class(
            ENGINE,
            "Actor",
            Some(&object),
            0x48,
            vec![
                field("Health", 0x30, PropertyType::Float, 4),
                bit("bHidden", 0x34, 0, 0x01),
                bit("bCanBeDamaged", 0x34, 0, 0x02),
                bit("bReplicates", 0x34, 0, 0x04),
                bit("bBlocking", 0x35, 0, 0x01),
                native_bool("bNative", 0x36),
                field(
                    "Mode",
                    0x37,
                    PropertyType::Byte {
                        path: path(CORE, "ENetRole"),
                    },
                    1,
                ),
                field(
                    "Location",
                    0x38,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    12,
                ),
            ],
            actor_functions(),
        ),
        class(
            ENGINE,
            "Pawn",
            Some(&actor),
            0x50,
            vec![field("Speed", 0x48, PropertyType::Float, 4)],
            Vec::new(),
        ),
        everything(),
        class(
            MODS,
            "BP_Thing_C",
            Some(&actor),
            0x50,
            vec![field("Count", 0x48, PropertyType::Int32, 4)],
            vec![function(
                &path(MODS, "BP_Thing_C"),
                "GetHome",
                12,
                vec![param(
                    "ReturnValue",
                    0,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    12,
                    CPF_RETURN_PARM,
                )],
            )],
        ),
    ];
    structs.extend(tricky());
    let enums = vec![
        enum_def(
            CORE,
            "ENetRole",
            1,
            &[
                ("ROLE_None", 0),
                ("ROLE_SimulatedProxy", 1),
                ("ROLE_AutonomousProxy", 2),
                ("ROLE_Authority", 3),
                ("ROLE_MAX", 4),
            ],
        ),
        // Signed values, keyword names, a repeated name and a repeated value.
        enum_def(
            CORE,
            "ESigned",
            4,
            &[
                ("Type", -1),
                ("Self", 0),
                ("Zero", 1),
                ("Zero", 2),
                ("Alias", 2),
                ("Minus One", 3),
            ],
        ),
        // 300 does not fit one byte.
        enum_def(
            ENGINE,
            "EWide",
            1,
            &[("Small", 1), ("Max", 255), ("TooBig", 300)],
        ),
        enum_def(ENGINE, "Direction", 2, &[("Left", 0), ("Right", 1)]),
    ];
    make_dump(layout, structs, enums)
}

/// A small dump with known byte offsets, for the behaviour test.
pub fn behaviour_dump() -> Dump {
    let layout = Profile::ue4_21_measured().layout;
    let object = path(CORE, "Object");
    let actor = path(ENGINE, "Actor");
    let structs = vec![
        class(CORE, "Object", None, 0x28, Vec::new(), Vec::new()),
        script_struct(
            CORE,
            "Vector",
            12,
            4,
            vec![
                field("X", 0, PropertyType::Float, 4),
                field("Y", 4, PropertyType::Float, 4),
                field("Z", 8, PropertyType::Float, 4),
            ],
        ),
        class(
            ENGINE,
            "Actor",
            Some(&object),
            0x40,
            vec![
                field("Health", 0x28, PropertyType::Float, 4),
                bit("bHidden", 0x2C, 0, 0x01),
                bit("bCanTick", 0x2C, 0, 0x02),
                field(
                    "Mode",
                    0x2D,
                    PropertyType::Byte {
                        path: path(ENGINE, "EMode"),
                    },
                    1,
                ),
                field(
                    "Location",
                    0x30,
                    PropertyType::Struct {
                        path: path(CORE, "Vector"),
                    },
                    12,
                ),
            ],
            vec![function(
                &actor,
                "SetHealth",
                0x10,
                vec![
                    param("NewHealth", 0, PropertyType::Float, 4, 0),
                    param("bClamp", 4, native_bool_type(), 1, 0),
                    param("OldHealth", 8, PropertyType::Float, 4, CPF_OUT_PARM),
                    param("ReturnValue", 12, native_bool_type(), 1, CPF_RETURN_PARM),
                ],
            )],
        ),
        class(
            ENGINE,
            "Pawn",
            Some(&actor),
            0x48,
            vec![field("Speed", 0x40, PropertyType::Float, 4)],
            Vec::new(),
        ),
    ];
    let enums = vec![enum_def(
        ENGINE,
        "EMode",
        1,
        &[("Walking", 0), ("Flying", 1), ("Swimming", 2)],
    )];
    make_dump(layout, structs, enums)
}

/// A dump built from the reflection truth of a synthetic engine image.
pub fn testkit_dump(profile: Profile) -> Dump {
    let fixture = FixtureBuilder::new(profile).filler_objects(0).build();
    make_dump(
        fixture.layout.clone(),
        fixture.truth.structs.clone(),
        fixture.truth.enums.clone(),
    )
}

/// The layout of a stock profile, for tests that need a plain one.
pub fn layout_of(profile: &Profile) -> EngineLayout {
    profile.layout.clone()
}
