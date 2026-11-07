//! What a caller can tell discovery.

use rayon::ThreadPool;
use ufathom_model::EngineVersion;
use ufathom_model::layout::TransformOp;

macro_rules! layout_overrides {
    ($($(#[$meta:meta])* $field:ident => $name:literal),* $(,)?) => {
        /// Per-field values a caller states instead of solving them.
        ///
        /// A value is still validated. If the target shows a different value
        /// for the field, discovery fails with
        /// [`ScanError::OverrideContradiction`](crate::ScanError::OverrideContradiction).
        /// If the target shows nothing for the field, the given value is used
        /// and checked by the global invariants at the end.
        ///
        /// Field names are the dotted names used by
        /// [`Evidence::field`](crate::Evidence::field).
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct LayoutOverrides {
            $($(#[$meta])* pub $field: Option<u32>,)*
        }

        impl LayoutOverrides {
            /// The override for the field with this dotted name.
            #[must_use]
            pub fn get(&self, name: &str) -> Option<u32> {
                match name {
                    $($name => self.$field,)*
                    _ => None,
                }
            }

            /// Every override that is set, with its dotted field name.
            pub fn iter(&self) -> impl Iterator<Item = (&'static str, u32)> + '_ {
                [$(($name, self.$field),)*]
                    .into_iter()
                    .filter_map(|(name, value)| value.map(|value| (name, value)))
            }

            /// Dotted names of every field that can be overridden.
            pub const NAMES: &'static [&'static str] = &[$($name,)*];
        }
    };
}

layout_overrides! {
    /// Offset of the `Objects` pointer in the object array, relative to the
    /// start of the lowest solved array field.
    objects_objects => "objects.objects",
    /// Offset of `MaxElements`.
    objects_max_elements => "objects.max_elements",
    /// Offset of `NumElements`.
    objects_num_elements => "objects.num_elements",
    /// Offset of `MaxChunks`.
    objects_max_chunks => "objects.max_chunks",
    /// Offset of `NumChunks`.
    objects_num_chunks => "objects.num_chunks",
    /// `sizeof(FUObjectItem)`.
    objects_item_size => "objects.item_size",
    /// Offset of the object pointer inside an item.
    objects_item_object => "objects.item_object",
    /// Offset of the serial number inside an item.
    objects_item_serial => "objects.item_serial",
    /// `sizeof(FName)`.
    fname_size => "fname.size",
    /// Offset of the comparison index inside an `FName`.
    fname_comparison_index => "fname.comparison_index",
    /// Offset of the display index inside an `FName`.
    fname_display_index => "fname.display_index",
    /// Offset of the number inside an `FName`.
    fname_number => "fname.number",
    /// Offset of `ObjectFlags`.
    uobject_flags => "uobject.flags",
    /// Offset of `InternalIndex`.
    uobject_index => "uobject.index",
    /// Offset of `ClassPrivate`.
    uobject_class => "uobject.class",
    /// Offset of `NamePrivate`.
    uobject_name => "uobject.name",
    /// Offset of `OuterPrivate`.
    uobject_outer => "uobject.outer",
    /// `sizeof(UObject)`.
    uobject_size => "uobject.size",
    /// Offset of `UField::Next`.
    ufield_next => "ufield.next",
    /// Offset of `UStruct::SuperStruct`.
    ustruct_super_struct => "ustruct.super_struct",
    /// Offset of `UStruct::Children`.
    ustruct_children => "ustruct.children",
    /// Offset of `UStruct::ChildProperties`.
    ustruct_child_properties => "ustruct.child_properties",
    /// Offset of `UStruct::PropertiesSize`.
    ustruct_properties_size => "ustruct.properties_size",
    /// Offset of `UStruct::MinAlignment`.
    ustruct_min_alignment => "ustruct.min_alignment",
    /// Width of `UStruct::MinAlignment`.
    ustruct_min_alignment_width => "ustruct.min_alignment_width",
    /// Offset of `UClass::ClassCastFlags`.
    uclass_cast_flags => "uclass.cast_flags",
    /// Offset of `UClass::ClassDefaultObject`.
    uclass_default_object => "uclass.default_object",
    /// Offset of `UFunction::FunctionFlags`.
    ufunction_flags => "ufunction.flags",
    /// Offset of `UFunction::Func`.
    ufunction_native_func => "ufunction.native_func",
    /// Offset of `ArrayDim` in a property.
    properties_array_dim => "properties.array_dim",
    /// Offset of `ElementSize` in a property.
    properties_element_size => "properties.element_size",
    /// Offset of `PropertyFlags` in a property.
    properties_flags => "properties.flags",
    /// Offset of `Offset_Internal` in a property.
    properties_offset_internal => "properties.offset_internal",
    /// `sizeof(UProperty)` or `sizeof(FProperty)`.
    properties_size => "properties.size",
    /// Offset of `ClassPrivate` in an `FField`.
    ffield_class => "ffield.class",
    /// Offset of `Next` in an `FField`.
    ffield_next => "ffield.next",
    /// Offset of `NamePrivate` in an `FField`.
    ffield_name => "ffield.name",
    /// Offset of `FlagsPrivate` in an `FField`.
    ffield_flags => "ffield.flags",
    /// Offset of the name in an `FFieldClass`.
    field_class_name => "field_class.name",
    /// Offset of `CastFlags` in an `FFieldClass`.
    field_class_cast_flags => "field_class.cast_flags",
    /// Number of low bits of a pool name index that address the offset in a block.
    names_block_offset_bits => "names.block_offset_bits",
}

/// Settings of [`discover`](crate::discover).
#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions<'a> {
    /// File name of the module that holds the engine globals. `None` selects
    /// the first `.exe` module the reader lists.
    pub module: Option<String>,
    /// Values the caller states instead of solving.
    pub overrides: LayoutOverrides,
    /// Steps that turn the stored `Objects` pointer into the real one. Empty
    /// for builds that store plain pointers. The caller supplies them; this
    /// crate never searches for keys.
    pub pointer_transform: Vec<TransformOp>,
    /// An engine version the caller believes the target to be. It only orders
    /// candidates when the evidence leaves a choice open, and never overrides
    /// the evidence.
    pub hint_version: Option<EngineVersion>,
    /// Pool for parallel name loading. The global pool is never used.
    pub pool: Option<&'a ThreadPool>,
}

impl DiscoverOptions<'_> {
    /// The hint as `major * 100 + minor`, the number engine eras are ordered by.
    pub(crate) fn hint_number(&self) -> Option<u32> {
        self.hint_version
            .map(|v| u32::from(v.major) * 100 + u32::from(v.minor))
    }
}
