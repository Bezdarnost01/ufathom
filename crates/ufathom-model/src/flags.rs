//! Named bits of the engine flag enums the rest of the project needs.
//!
//! The values are stable across the engine versions this project targets
//! (UE 4.21 and later). Names follow the engine, with the `E...::` prefix
//! turned into a `CPF_`, `FUNC_` or `RF_` constant prefix.

/// `EPropertyFlags` bits, stored as `u64`.
pub mod property {
    /// `CPF_Edit`: visible and editable in the editor.
    pub const CPF_EDIT: u64 = 0x1;
    /// `CPF_ConstParm`: a `const` function parameter.
    pub const CPF_CONST_PARM: u64 = 0x2;
    /// `CPF_BlueprintVisible`: accessible from Blueprints.
    pub const CPF_BLUEPRINT_VISIBLE: u64 = 0x4;
    /// `CPF_ExportObject`: object is exported by value.
    pub const CPF_EXPORT_OBJECT: u64 = 0x8;
    /// `CPF_BlueprintReadOnly`: Blueprints cannot modify it.
    pub const CPF_BLUEPRINT_READ_ONLY: u64 = 0x10;
    /// `CPF_Net`: replicated.
    pub const CPF_NET: u64 = 0x20;
    /// `CPF_Parm`: a function parameter.
    pub const CPF_PARM: u64 = 0x80;
    /// `CPF_OutParm`: an output parameter.
    pub const CPF_OUT_PARM: u64 = 0x100;
    /// `CPF_ReturnParm`: the return value.
    pub const CPF_RETURN_PARM: u64 = 0x400;
    /// `CPF_Transient`: not saved.
    pub const CPF_TRANSIENT: u64 = 0x2000;
    /// `CPF_ReferenceParm`: passed by reference.
    pub const CPF_REFERENCE_PARM: u64 = 0x800_0000;
}

/// `EFunctionFlags` bits, stored as `u32`.
pub mod function {
    /// `FUNC_Final`: cannot be overridden.
    pub const FUNC_FINAL: u32 = 0x1;
    /// `FUNC_Net`: replicated over the network.
    pub const FUNC_NET: u32 = 0x40;
    /// `FUNC_NetReliable`: replicated reliably.
    pub const FUNC_NET_RELIABLE: u32 = 0x80;
    /// `FUNC_Native`: implemented in C++.
    pub const FUNC_NATIVE: u32 = 0x400;
    /// `FUNC_Static`: has no `this`.
    pub const FUNC_STATIC: u32 = 0x2000;
    /// `FUNC_NetMulticast`: runs on the server and all clients.
    pub const FUNC_NET_MULTICAST: u32 = 0x4000;
    /// `FUNC_NetServer`: runs on the server.
    pub const FUNC_NET_SERVER: u32 = 0x20_0000;
    /// `FUNC_HasOutParms`: has output or reference parameters.
    pub const FUNC_HAS_OUT_PARMS: u32 = 0x40_0000;
    /// `FUNC_NetClient`: runs on the owning client.
    pub const FUNC_NET_CLIENT: u32 = 0x100_0000;
    /// `FUNC_BlueprintCallable`: callable from Blueprints.
    pub const FUNC_BLUEPRINT_CALLABLE: u32 = 0x400_0000;
    /// `FUNC_BlueprintEvent`: overridable in Blueprints.
    pub const FUNC_BLUEPRINT_EVENT: u32 = 0x800_0000;
    /// `FUNC_BlueprintPure`: no side effects.
    pub const FUNC_BLUEPRINT_PURE: u32 = 0x1000_0000;
    /// `FUNC_Const`: does not modify `this`.
    pub const FUNC_CONST: u32 = 0x4000_0000;

    /// All bits that say where a function runs on the network. Not an engine
    /// constant: this crate uses it to compare the network behaviour of two
    /// builds.
    pub const FUNC_NETWORK_MASK: u32 =
        FUNC_NET | FUNC_NET_SERVER | FUNC_NET_CLIENT | FUNC_NET_MULTICAST;
}

/// `EObjectFlags` bits, stored as `u32`.
pub mod object {
    /// `RF_Public`: visible outside its package.
    pub const RF_PUBLIC: u32 = 0x1;
    /// `RF_Standalone`: kept alive without references.
    pub const RF_STANDALONE: u32 = 0x2;
    /// `RF_ClassDefaultObject`: the `Default__` object of a class.
    pub const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
    /// `RF_ArchetypeObject`: template for instances.
    pub const RF_ARCHETYPE_OBJECT: u32 = 0x20;
    /// `RF_Transient`: not saved.
    pub const RF_TRANSIENT: u32 = 0x40;
}

pub use function::*;
pub use object::*;
pub use property::*;
