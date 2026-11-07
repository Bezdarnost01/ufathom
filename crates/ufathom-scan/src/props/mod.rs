//! Step 5: properties.
//!
//! The property system is told by where `Guid`'s members hang: a pointer that
//! is not an object whose chain yields `A, B, C, D` is an `FField` list
//! (modern); otherwise the `Children` list yields the members as objects
//! (legacy). The core fields then come from the same anchors, typed payloads
//! come from the instances of each property kind, and `FName`'s size comes
//! from the element size of a `NameProperty`.

mod core;
mod modern;
mod typed;

use std::collections::HashSet;

use ufathom_engine::NameTable;
use ufathom_model::layout::{
    FFieldClassLayout, FFieldLayout, FNameLayout, PropertyLayout, PropertySystem,
};

use crate::ctx::Ctx;
use crate::error::ScanError;
use crate::limits::{MAX_CHAIN_STEPS, MAX_PROPERTY_SAMPLES};
use crate::structs::{Sizes, StructFields};
use crate::universe::{Anchors, Obj, Universe};

pub(crate) use modern::ModernOffsets;

/// What the property steps read through.
pub(crate) struct Env<'e, 'a> {
    pub ctx: &'e Ctx<'a>,
    pub universe: &'e Universe,
    pub table: &'e NameTable,
    pub fname: FNameLayout,
    pub name_offset: u32,
    pub anchors: &'e Anchors,
    pub fields: &'e StructFields,
    pub sizes: Sizes,
}

impl Env<'_, '_> {
    /// An `FName` at `addr`, as text with its instance number.
    ///
    /// Every index of the `FName` must resolve, and the comparison and display
    /// entries must be one name up to case: a struct read at the wrong offset
    /// can place its display index on the true comparison index and still
    /// decode.
    pub(crate) fn fname_at(&self, addr: u64) -> Option<String> {
        let bytes = self.ctx.read(addr, self.fname.size as usize)?;
        let comparison = crate::ctx::le_u32(&bytes, self.fname.comparison_index as usize)?;
        let comparison_text = self.table.resolve_index(comparison)?;
        if let Some(offset) = self.fname.display_index {
            let display = crate::ctx::le_u32(&bytes, offset as usize)?;
            // The two entries are one name, spelled with and without case.
            if !self
                .table
                .resolve_index(display)?
                .eq_ignore_ascii_case(comparison_text)
            {
                return None;
            }
        }
        self.table.resolve(&self.fname, &bytes).ok()
    }
}

/// The two property systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum System {
    Legacy,
    Modern(ModernOffsets),
}

/// How a sampled property is owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// A member of a class or struct.
    Member,
    /// A parameter of a function.
    Param,
}

/// One property reached from a struct or function.
#[derive(Debug, Clone)]
pub(crate) struct Prop {
    pub addr: u64,
    pub owner: u64,
    pub role: Role,
    pub name: String,
    pub class_name: String,
}

impl System {
    /// The members of `owner` in chain order, as addresses.
    pub(crate) fn chain(&self, env: &Env<'_, '_>, owner: u64) -> Vec<u64> {
        let (head_offset, next) = match self {
            Self::Legacy => (env.fields.ustruct.children, env.fields.ufield.next),
            Self::Modern(m) => (m.child_properties, m.next),
        };
        let Some(mut current) = env.ctx.u64(owner + u64::from(head_offset)) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while current != 0 && out.len() < MAX_CHAIN_STEPS && seen.insert(current) {
            out.push(current);
            match env.ctx.u64(current + u64::from(next)) {
                Some(link) => current = link,
                None => break,
            }
        }
        out
    }

    /// The member name of a property.
    pub(crate) fn name(&self, env: &Env<'_, '_>, prop: u64) -> Option<String> {
        let at = match self {
            Self::Legacy => env.name_offset,
            Self::Modern(m) => m.name,
        };
        env.fname_at(prop + u64::from(at))
    }

    /// The class name of a property, such as `IntProperty`.
    pub(crate) fn class_name(&self, env: &Env<'_, '_>, prop: u64) -> Option<String> {
        match self {
            Self::Legacy => {
                let class = env.universe.get(prop)?.class;
                env.universe.name_of(env.table, class).map(str::to_owned)
            }
            Self::Modern(m) => {
                let class = env.ctx.u64(prop + u64::from(m.class))?;
                env.fname_at(class + u64::from(m.fc_name))
            }
        }
    }

    /// Whether a legacy chain member is a property: its class name says so.
    fn is_property(&self, env: &Env<'_, '_>, member: u64) -> bool {
        match self {
            Self::Modern(_) => true,
            Self::Legacy => self
                .class_name(env, member)
                .is_some_and(|name| name.contains("Property")),
        }
    }

    /// The properties of `owner`, named and classified.
    pub(crate) fn props_of(&self, env: &Env<'_, '_>, owner: u64, role: Role) -> Vec<Prop> {
        self.chain(env, owner)
            .into_iter()
            .filter(|&member| self.is_property(env, member))
            .filter_map(|addr| {
                Some(Prop {
                    addr,
                    owner,
                    role,
                    name: self.name(env, addr)?,
                    class_name: self.class_name(env, addr)?,
                })
            })
            .collect()
    }
}

/// The properties of every struct and function, up to a bound.
pub(crate) fn sample_props(env: &Env<'_, '_>, system: &System) -> Vec<Prop> {
    let mut out = Vec::new();
    let structs: Vec<Obj> = crate::structs::structs_of(env.universe, env.table, env.anchors);
    for owner in &structs {
        out.extend(system.props_of(env, owner.addr, Role::Member));
        if out.len() >= MAX_PROPERTY_SAMPLES {
            return out;
        }
    }
    for function in env
        .universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.function)
    {
        out.extend(system.props_of(env, function.addr, Role::Param));
        if out.len() >= MAX_PROPERTY_SAMPLES {
            break;
        }
    }
    out
}

/// Everything the property steps solved.
pub(crate) struct Solved {
    pub layout: PropertyLayout,
    pub child_properties: Option<u32>,
    pub fname_size: u32,
    pub props: Vec<Prop>,
}

/// Solves the property system, the core fields, the typed payloads and the
/// size of `FName`.
pub(crate) fn solve(env: &Env<'_, '_>) -> Result<Solved, ScanError> {
    let system = modern::detect(env)?;
    let props = sample_props(env, &system);
    if props.len() < 8 {
        return Err(ScanError::missing(
            "properties.array_dim",
            format!("only {} properties could be walked", props.len()),
        ));
    }
    let core = core::solve_core(env, &system, &props)?;
    let typed = typed::solve_typed(env, &system, &props, &core)?;
    let size = typed::solve_size(env, &system, &core, &typed)?;
    let (system, field_layout) = match system {
        System::Legacy => (System::Legacy, PropertySystem::Legacy),
        System::Modern(m) => {
            let ffield_flags = modern::solve_ffield_flags(env, &m, &props, &core, size)?;
            let cast_flags = modern::solve_field_class_cast_flags(env, &m, &props)?;
            (
                System::Modern(m),
                PropertySystem::Modern {
                    ffield: FFieldLayout {
                        class: m.class,
                        next: m.next,
                        name: m.name,
                        flags: ffield_flags,
                    },
                    field_class: FFieldClassLayout {
                        name: m.fc_name,
                        cast_flags,
                    },
                },
            )
        }
    };
    let layout = PropertyLayout {
        system: field_layout,
        array_dim: core.array_dim,
        element_size: core.element_size,
        flags: core.flags,
        offset_internal: core.offset_internal,
        size,
        typed: typed.layout,
    };
    let fname_size = typed::solve_fname_size(env, &props, &core)?;
    Ok(Solved {
        child_properties: match system {
            System::Modern(m) => Some(m.child_properties),
            System::Legacy => None,
        },
        layout,
        fname_size,
        props,
    })
}
