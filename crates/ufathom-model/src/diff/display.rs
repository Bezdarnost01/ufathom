//! Human-readable report of a diff.

use std::fmt::{self, Display, Formatter};

use crate::flags::{FUNC_NET, FUNC_NET_CLIENT, FUNC_NET_MULTICAST, FUNC_NET_SERVER};

use super::report::{
    Ambiguity, AmbiguousSubject, DumpDiff, EnumChange, FunctionChange, ParamChange, ParamFlag,
    SlotChange, TypeChange, TypeKey,
};

impl Display for TypeKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.package, self.name)
    }
}

impl Display for DumpDiff {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return writeln!(f, "No differences.");
        }
        write_keys(f, "Types added", &self.added_types)?;
        write_keys(f, "Types removed", &self.removed_types)?;
        if !self.changed_types.is_empty() {
            writeln!(f, "Types changed ({}):", self.changed_types.len())?;
            for changed in &self.changed_types {
                writeln!(f, "  {}", changed.key)?;
                for change in &changed.changes {
                    writeln!(f, "    {change}")?;
                }
            }
        }
        write_keys(f, "Enums added", &self.added_enums)?;
        write_keys(f, "Enums removed", &self.removed_enums)?;
        if !self.changed_enums.is_empty() {
            writeln!(f, "Enums changed ({}):", self.changed_enums.len())?;
            for changed in &self.changed_enums {
                writeln!(f, "  {}", changed.key)?;
                for change in &changed.changes {
                    writeln!(f, "    {change}")?;
                }
            }
        }
        if !self.ambiguities.is_empty() {
            writeln!(f, "Ambiguous, not compared ({}):", self.ambiguities.len())?;
            for ambiguity in &self.ambiguities {
                writeln!(f, "  {ambiguity}")?;
            }
        }
        Ok(())
    }
}

fn write_keys(f: &mut Formatter<'_>, title: &str, keys: &[TypeKey]) -> fmt::Result {
    if keys.is_empty() {
        return Ok(());
    }
    writeln!(f, "{title} ({}):", keys.len())?;
    for key in keys {
        writeln!(f, "  {key}")?;
    }
    Ok(())
}

/// Writes `items` separated by `; `.
fn write_joined<T: Display>(f: &mut Formatter<'_>, items: &[T]) -> fmt::Result {
    for (position, item) in items.iter().enumerate() {
        if position > 0 {
            f.write_str("; ")?;
        }
        item.fmt(f)?;
    }
    Ok(())
}

impl Display for Ambiguity {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.subject {
            AmbiguousSubject::Type => write!(f, "type {}", self.name)?,
            AmbiguousSubject::Enum => write!(f, "enum {}", self.name)?,
            AmbiguousSubject::Field { owner } => write!(f, "field {} in {owner}", self.name)?,
            AmbiguousSubject::Function { owner } => {
                write!(f, "function {} in {owner}", self.name)?;
            }
            AmbiguousSubject::AncestorFunction { owner } => {
                write!(f, "function {} in ancestor {owner}", self.name)?;
            }
            AmbiguousSubject::EnumValue { owner } => {
                write!(f, "enumerator {} in {owner}", self.name)?;
            }
            AmbiguousSubject::AncestorPath => write!(f, "parent path {}", self.name)?,
        }
        match self.subject {
            AmbiguousSubject::AncestorPath | AmbiguousSubject::AncestorFunction { .. } => {
                write!(f, ": {} matches in new", self.new_count)
            }
            _ => write!(f, ": {} in old, {} in new", self.old_count, self.new_count),
        }
    }
}

impl Display for TypeChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::KindChanged { old, new } => write!(f, "kind: {old:?} -> {new:?}"),
            Self::ParentChanged { old, new } => write!(
                f,
                "parent: {} -> {}",
                old.as_deref().unwrap_or("none"),
                new.as_deref().unwrap_or("none")
            ),
            Self::SizeChanged { old, new } => write!(f, "size: {old:#x} -> {new:#x}"),
            Self::AlignmentChanged { old, new } => write!(f, "alignment: {old} -> {new}"),
            Self::FieldAdded { name, offset } => write!(f, "field {name} added at {offset:#x}"),
            Self::FieldRemoved { name, offset } => {
                write!(f, "field {name} removed (was at {offset:#x})")
            }
            Self::FieldChanged { name, changes } => {
                write!(f, "field {name}: ")?;
                write_joined(f, changes)
            }
            Self::FunctionAdded { name } => write!(f, "function {name} added"),
            Self::FunctionMoved { name, to } => write!(f, "function {name} moved to {to}"),
            Self::FunctionRemoved { name } => write!(f, "function {name} removed"),
            Self::FunctionChanged { name, changes } => {
                write!(f, "function {name}: ")?;
                write_joined(f, changes)
            }
        }
    }
}

impl Display for SlotChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offset { old, new } => write!(f, "offset {old:#x} -> {new:#x}"),
            Self::Type { old, new } => write!(f, "type {old} -> {new}"),
            Self::BoolBits { old, new } => write!(f, "bit layout {old} -> {new}"),
            Self::ElementSize { old, new } => write!(f, "element size {old} -> {new}"),
            Self::ArrayDim { old, new } => write!(f, "array dim {old} -> {new}"),
        }
    }
}

impl Display for ParamFlag {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Out => "out",
            Self::Return => "return",
            Self::Reference => "reference",
            Self::Const => "const",
        })
    }
}

impl Display for ParamChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot(change) => change.fmt(f),
            Self::Flag { flag, old, new } => write!(f, "{flag} flag {old} -> {new}"),
        }
    }
}

impl Display for FunctionChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParamsSize { old, new } => write!(f, "params size {old:#x} -> {new:#x}"),
            Self::NetworkFlags { old, new } => {
                write!(
                    f,
                    "network {} -> {}",
                    network_names(*old),
                    network_names(*new)
                )
            }
            Self::ParamCount { old, new } => write!(f, "parameter count {old} -> {new}"),
            Self::Param {
                index,
                name,
                changes,
            } => {
                write!(f, "parameter #{index} ({name}): ")?;
                write_joined(f, changes)
            }
        }
    }
}

/// Names the network bits set in `bits`, or `none`.
fn network_names(bits: u32) -> String {
    const NAMES: [(u32, &str); 4] = [
        (FUNC_NET, "Net"),
        (FUNC_NET_SERVER, "NetServer"),
        (FUNC_NET_CLIENT, "NetClient"),
        (FUNC_NET_MULTICAST, "NetMulticast"),
    ];
    let set: Vec<&str> = NAMES
        .iter()
        .filter(|(mask, _)| bits & mask != 0)
        .map(|&(_, name)| name)
        .collect();
    if set.is_empty() {
        "none".to_owned()
    } else {
        set.join("|")
    }
}

impl Display for EnumChange {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnderlyingSizeChanged { old, new } => {
                write!(f, "underlying size {old} -> {new}")
            }
            Self::ValueAdded { name, value } => write!(f, "{name} added = {value}"),
            Self::ValueRemoved { name, value } => write!(f, "{name} removed (was {value})"),
            Self::ValueChanged { name, old, new } => write!(f, "{name}: {old} -> {new}"),
        }
    }
}
