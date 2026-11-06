//! The data a diff produces.

use serde::{Deserialize, Serialize};

use crate::dump::{BoolBits, PropertyType, StructKind};

/// Identity of a type or enum across two dumps: package and short name.
///
/// The full path is not used because it may embed an outer chain that differs
/// between builds for the same logical type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TypeKey {
    /// Package, for example `/Script/Engine`.
    pub package: String,
    /// Short name, for example `Actor`.
    pub name: String,
}

impl TypeKey {
    /// Builds a key from a package and a short name.
    #[must_use]
    pub fn new(package: &str, name: &str) -> Self {
        Self {
            package: package.to_owned(),
            name: name.to_owned(),
        }
    }
}

/// The result of comparing two dumps.
///
/// Every list is sorted, so the same two dumps always give the same diff.
/// Addresses, member order and `native_rva` never appear in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpDiff {
    /// Types only in the new dump.
    pub added_types: Vec<TypeKey>,
    /// Types only in the old dump.
    pub removed_types: Vec<TypeKey>,
    /// Types in both dumps that differ.
    pub changed_types: Vec<TypeDiff>,
    /// Enums only in the new dump.
    pub added_enums: Vec<TypeKey>,
    /// Enums only in the old dump.
    pub removed_enums: Vec<TypeKey>,
    /// Enums in both dumps that differ.
    pub changed_enums: Vec<EnumDiff>,
    /// Names that could not be compared because they occur more than once.
    /// The diff never picks one of the candidates.
    pub ambiguities: Vec<Ambiguity>,
}

impl DumpDiff {
    /// `true` when the two dumps are equivalent for porting purposes: no
    /// difference and no ambiguity.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added_types.is_empty()
            && self.removed_types.is_empty()
            && self.changed_types.is_empty()
            && self.added_enums.is_empty()
            && self.removed_enums.is_empty()
            && self.changed_enums.is_empty()
            && self.ambiguities.is_empty()
    }
}

/// What a name refers to when it is ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AmbiguousSubject {
    /// A struct or class, matched by package and name. The `name` of the
    /// [`Ambiguity`] is `package.name`.
    Type,
    /// An enum, matched by package and name.
    Enum,
    /// A field name inside a type.
    Field {
        /// The type that holds the field.
        owner: TypeKey,
    },
    /// A function name inside a type.
    Function {
        /// The type that holds the functions.
        owner: TypeKey,
    },
    /// A function name that occurs more than once in an ancestor searched
    /// for a function missing from a class. Only the new dump is searched,
    /// so `old_count` is 0.
    AncestorFunction {
        /// The ancestor that holds the functions.
        owner: TypeKey,
    },
    /// An enumerator name inside an enum.
    EnumValue {
        /// The enum that holds the enumerator.
        owner: TypeKey,
    },
    /// A parent path in the new dump that matches several types, so the
    /// ancestor search could not continue. The `name` is the path.
    AncestorPath,
}

/// A name that occurs more than once on at least one side.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ambiguity {
    /// What kind of name this is and where it lives.
    pub subject: AmbiguousSubject,
    /// The ambiguous name.
    pub name: String,
    /// Occurrences in the old dump.
    pub old_count: usize,
    /// Occurrences in the new dump. For [`AmbiguousSubject::AncestorPath`] and
    /// [`AmbiguousSubject::AncestorFunction`] the old dump is not consulted,
    /// so `old_count` is 0.
    pub new_count: usize,
}

/// Differences of one type present in both dumps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDiff {
    /// The type.
    pub key: TypeKey,
    /// Changes in a fixed order: kind, parent, size, alignment, fields by
    /// name, functions by name.
    pub changes: Vec<TypeChange>,
}

/// One difference inside a type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeChange {
    /// Class became a script struct or the reverse.
    KindChanged {
        /// Kind in the old dump.
        old: StructKind,
        /// Kind in the new dump.
        new: StructKind,
    },
    /// The parent type changed.
    ParentChanged {
        /// Parent path in the old dump.
        old: Option<String>,
        /// Parent path in the new dump.
        new: Option<String>,
    },
    /// `PropertiesSize` changed.
    SizeChanged {
        /// Size in the old dump.
        old: u32,
        /// Size in the new dump.
        new: u32,
    },
    /// Minimum alignment changed.
    AlignmentChanged {
        /// Alignment in the old dump.
        old: u32,
        /// Alignment in the new dump.
        new: u32,
    },
    /// A field exists only in the new dump.
    FieldAdded {
        /// Field name.
        name: String,
        /// Offset in the new dump.
        offset: u32,
    },
    /// A field exists only in the old dump.
    FieldRemoved {
        /// Field name.
        name: String,
        /// Offset in the old dump.
        offset: u32,
    },
    /// A field exists in both dumps with a different layout.
    FieldChanged {
        /// Field name.
        name: String,
        /// What differs.
        changes: Vec<SlotChange>,
    },
    /// A function exists only in the new dump.
    FunctionAdded {
        /// Function name.
        name: String,
    },
    /// A function is gone from this class but still reachable through an
    /// ancestor in the new dump.
    FunctionMoved {
        /// Function name.
        name: String,
        /// The ancestor that now declares it.
        to: TypeKey,
    },
    /// A function exists only in the old dump and in no ancestor of the new
    /// class.
    FunctionRemoved {
        /// Function name.
        name: String,
    },
    /// A function's ABI differs. For a moved function this compares the old
    /// function with the one found in the ancestor.
    FunctionChanged {
        /// Function name.
        name: String,
        /// What differs.
        changes: Vec<FunctionChange>,
    },
}

/// A difference in where and how a value is stored. Shared by data members and
/// by function parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotChange {
    /// The offset changed.
    Offset {
        /// Offset in the old dump.
        old: u32,
        /// Offset in the new dump.
        new: u32,
    },
    /// The type changed.
    Type {
        /// Type in the old dump.
        old: PropertyType,
        /// Type in the new dump.
        new: PropertyType,
    },
    /// Both are `bool` but the byte offset or a mask differs.
    BoolBits {
        /// Layout in the old dump.
        old: BoolBits,
        /// Layout in the new dump.
        new: BoolBits,
    },
    /// The element size changed.
    ElementSize {
        /// Size in the old dump.
        old: u32,
        /// Size in the new dump.
        new: u32,
    },
    /// The element count of a C array changed.
    ArrayDim {
        /// Count in the old dump.
        old: u32,
        /// Count in the new dump.
        new: u32,
    },
}

/// A parameter flag that is part of the calling convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ParamFlag {
    /// `CPF_OutParm`.
    Out,
    /// `CPF_ReturnParm`.
    Return,
    /// `CPF_ReferenceParm`.
    Reference,
    /// `CPF_ConstParm`.
    Const,
}

/// One difference in a function's ABI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionChange {
    /// The size of the parameter block changed.
    ParamsSize {
        /// Size in the old dump.
        old: u32,
        /// Size in the new dump.
        new: u32,
    },
    /// The network flags changed. Only `FUNC_Net`, `FUNC_NetServer`,
    /// `FUNC_NetClient` and `FUNC_NetMulticast` are kept in the values.
    NetworkFlags {
        /// Network bits in the old dump.
        old: u32,
        /// Network bits in the new dump.
        new: u32,
    },
    /// The number of parameters changed. The common prefix is still compared.
    ParamCount {
        /// Count in the old dump.
        old: usize,
        /// Count in the new dump.
        new: usize,
    },
    /// A parameter, matched by position, has a different layout. A rename is
    /// not a change, so `name` is informational and comes from the new dump.
    Param {
        /// Zero-based position.
        index: usize,
        /// Parameter name in the new dump.
        name: String,
        /// What differs.
        changes: Vec<ParamChange>,
    },
}

/// One difference in a function parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamChange {
    /// Offset, type or size differs.
    Slot(SlotChange),
    /// A flag was set or cleared.
    Flag {
        /// Which flag.
        flag: ParamFlag,
        /// Value in the old dump.
        old: bool,
        /// Value in the new dump.
        new: bool,
    },
}

/// Differences of one enum present in both dumps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumDiff {
    /// The enum.
    pub key: TypeKey,
    /// Changes in a fixed order: underlying size, then enumerators by name.
    pub changes: Vec<EnumChange>,
}

/// One difference inside an enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnumChange {
    /// The width of the underlying integer changed.
    UnderlyingSizeChanged {
        /// Width in the old dump.
        old: u32,
        /// Width in the new dump.
        new: u32,
    },
    /// An enumerator exists only in the new dump.
    ValueAdded {
        /// Enumerator name.
        name: String,
        /// Its value.
        value: i64,
    },
    /// An enumerator exists only in the old dump.
    ValueRemoved {
        /// Enumerator name.
        name: String,
        /// Its value.
        value: i64,
    },
    /// An enumerator has a different value.
    ValueChanged {
        /// Enumerator name.
        name: String,
        /// Value in the old dump.
        old: i64,
        /// Value in the new dump.
        new: i64,
    },
}
