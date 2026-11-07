//! Turns a dump into planned types: names, layouts, accessors and wrappers.
//!
//! Nothing here writes text. [`crate::emit`] prints what this module decides.

use std::collections::{BTreeMap, BTreeSet};

use ufathom_model::flags::{CPF_CONST_PARM, CPF_OUT_PARM, CPF_RETURN_PARM};
use ufathom_model::{Dump, EnumDef, FieldDef, FunctionDef, PropertyType, StructDef, StructKind};

use crate::names::{self, Namespace};
use crate::planner::{BaseInfo, BaseMode, FieldKind, Item, Note, Plan, PlanField, PlanInput, plan};
use crate::report::{Degradation, DegradeReason};
use crate::types::{EntryKind, FNameShape, NameEntry, Resolved, TypeTable, enum_int, fname_shape};

/// Alignment declared for a function parameter block.
const PARAM_ALIGN: u32 = 16;

/// A module of the generated crate.
#[derive(Debug)]
pub(crate) struct ModuleInfo {
    /// Dump path of the package.
    pub package: String,
    /// Module and feature name.
    pub name: String,
    /// Modules this one needs, as feature names, sorted.
    pub deps: BTreeSet<String>,
}

/// Getter and setter names of a bit-field `bool`.
#[derive(Debug, Clone)]
pub(crate) struct BitAccess {
    pub getter: String,
    pub setter: String,
    /// Index of the byte inside the storage.
    pub storage_byte: u32,
    /// Name of the storage field.
    pub storage_field: String,
    /// Mask in the byte.
    pub mask: u8,
    /// Byte offset from the start of the owner.
    pub abs_offset: u32,
}

/// One field of a type or parameter block, with everything decided.
#[derive(Debug)]
pub(crate) struct PlannedField {
    /// Name in the dump.
    pub name: String,
    /// Offset in the dump.
    pub offset: u32,
    /// Size of one element in the dump.
    pub element_size: u32,
    /// Number of elements.
    pub count: u32,
    /// The dump type as text.
    pub ty_display: String,
    /// Rust type, for a typed field.
    pub resolved: Option<Resolved>,
    /// Name of the Rust struct field, if the field has storage.
    pub rust_field: Option<String>,
    /// Name of the `FieldRef` constant. `None` when none is generated.
    pub const_name: Option<String>,
    /// Accessors, for a bit-field `bool`.
    pub bit: Option<BitAccess>,
    /// `true` for a bit-field `bool` (with or without storage).
    pub is_bit: bool,
    /// Raw mask and byte offset of a bit-field `bool`.
    pub bit_raw: Option<(u8, u8)>,
    /// Where the field is a degraded byte array.
    pub is_bytes: bool,
    /// Why the field is degraded or left out, if it is.
    pub note: Option<String>,
}

/// How a parameter reaches the wrapper.
#[derive(Debug, Clone)]
pub(crate) struct ParamBinding {
    /// Index into the planned fields.
    pub field: usize,
    /// Name of the wrapper argument.
    pub arg: String,
    /// The type shown in the wrapper signature.
    pub api_type: String,
    /// `true` when the value is a `UBool` seen as `bool`.
    pub is_ubool: bool,
}

/// A function with a wrapper.
#[derive(Debug)]
pub(crate) struct PlannedFunction {
    /// Path of the function.
    pub path: String,
    /// Wrapper name.
    pub wrapper: String,
    /// Name of the parameter struct.
    pub params_struct: String,
    /// Size of the parameter block.
    pub size: u32,
    /// Layout of the parameter block.
    pub plan: Plan,
    /// The parameters as fields.
    pub fields: Vec<PlannedField>,
    /// Parameters the caller passes, in declared order.
    pub inputs: Vec<ParamBinding>,
    /// The return value, if any.
    pub ret: Option<ParamBinding>,
    /// Out parameters, in declared order.
    pub outs: Vec<ParamBinding>,
    /// Modules referenced.
    pub refs: BTreeSet<String>,
}

/// A class or script struct, planned.
#[derive(Debug)]
pub(crate) struct PlannedStruct {
    /// Path in the dump.
    pub path: String,
    /// Module.
    pub module: String,
    /// Rust name.
    pub rust_name: String,
    /// `true` for a class.
    pub is_class: bool,
    /// Size.
    pub size: u32,
    /// Layout.
    pub plan: Plan,
    /// The fields in the dump's order.
    pub fields: Vec<PlannedField>,
    /// Rust path of the parent, if it is generated.
    pub base_path: Option<String>,
    /// Rust paths of every ancestor, nearest first.
    pub ancestors: Vec<String>,
    /// Functions with wrappers.
    pub functions: Vec<PlannedFunction>,
    /// Modules referenced.
    pub refs: BTreeSet<String>,
}

/// An enum, planned.
#[derive(Debug)]
pub(crate) struct PlannedEnum {
    /// Path in the dump.
    pub path: String,
    /// Module.
    pub module: String,
    /// Rust name.
    pub rust_name: String,
    /// Underlying integer type.
    pub int: &'static str,
    /// `(constant name, value)`.
    pub consts: Vec<(String, i64)>,
    /// `(value, enumerator name)` for `name()`, one per value.
    pub names: Vec<(i64, String)>,
}

/// Everything decided about a dump.
#[derive(Debug)]
pub(crate) struct Built {
    pub modules: Vec<ModuleInfo>,
    pub structs: Vec<PlannedStruct>,
    pub enums: Vec<PlannedEnum>,
    pub fname: FNameShape,
    pub degraded: Vec<Degradation>,
    pub fields: usize,
    pub functions: usize,
    pub functions_skipped: usize,
}

fn enum_prefix(name: &str) -> &'static str {
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some('E'), Some(second)) if second.is_ascii_uppercase() => "",
        _ => "E",
    }
}

/// Whether the class or one of its ancestors is named `Actor`.
fn is_actor_derived(def: &StructDef, by_path: &BTreeMap<&str, usize>, defs: &[&StructDef]) -> bool {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut current = Some(def);
    while let Some(c) = current {
        if c.name == "Actor" {
            return true;
        }
        if !seen.insert(c.path.as_str()) {
            return false;
        }
        current = c
            .super_path
            .as_deref()
            .and_then(|p| by_path.get(p))
            .and_then(|&i| defs.get(i).copied());
    }
    false
}

/// Plans a whole dump.
#[allow(
    clippy::too_many_lines,
    reason = "one linear pipeline; splitting it would only pass the same state around"
)]
pub(crate) fn build(dump: &Dump) -> Built {
    let fname = fname_shape(&dump.layout.fname);

    // Packages and module names.
    let mut packages: BTreeSet<&str> = dump.packages.iter().map(String::as_str).collect();
    packages.extend(dump.structs.iter().map(|s| s.package.as_str()));
    packages.extend(dump.enums.iter().map(|e| e.package.as_str()));
    let mut module_ns = Namespace::with_reserved(&["default", "all"]);
    let mut module_of: BTreeMap<&str, String> = BTreeMap::new();
    for pkg in &packages {
        module_of.insert(
            pkg,
            module_ns.claim(&names::module_name(names::last_segment(pkg))),
        );
    }

    // Definitions in a fixed order.
    let mut defs: Vec<&StructDef> = dump.structs.iter().collect();
    defs.sort();
    let mut enum_defs: Vec<&EnumDef> = dump.enums.iter().collect();
    enum_defs.sort();
    let mut by_path: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, d) in defs.iter().enumerate() {
        by_path.entry(d.path.as_str()).or_insert(i);
    }

    // Type names, per module, in path order.
    let mut type_ns: BTreeMap<String, Namespace> = BTreeMap::new();
    let mut struct_names: Vec<String> = Vec::new();
    let mut table = TypeTable {
        fname: fname.clone(),
        names: BTreeMap::new(),
        layouts: BTreeMap::new(),
        enum_ints: BTreeMap::new(),
    };
    for (i, d) in defs.iter().enumerate() {
        let module = module_of
            .get(d.package.as_str())
            .cloned()
            .unwrap_or_default();
        let prefix = match d.kind {
            StructKind::ScriptStruct => "F",
            StructKind::Class => {
                if is_actor_derived(d, &by_path, &defs) {
                    "A"
                } else {
                    "U"
                }
            }
        };
        let name = type_ns
            .entry(module.clone())
            .or_default()
            .claim(&format!("{prefix}{}", names::sanitize(&d.name)));
        struct_names.push(name.clone());
        if by_path.get(d.path.as_str()) == Some(&i) {
            table.names.insert(
                d.path.clone(),
                NameEntry {
                    module,
                    name,
                    kind: match d.kind {
                        StructKind::Class => EntryKind::Class,
                        StructKind::ScriptStruct => EntryKind::Struct,
                    },
                },
            );
        }
    }
    let mut enums: Vec<PlannedEnum> = Vec::new();
    let mut degraded: Vec<Degradation> = Vec::new();
    let mut seen_enum: BTreeSet<&str> = BTreeSet::new();
    for e in &enum_defs {
        let module = module_of
            .get(e.package.as_str())
            .cloned()
            .unwrap_or_default();
        let rust_name = type_ns.entry(module.clone()).or_default().claim(&format!(
            "{}{}",
            enum_prefix(&e.name),
            names::sanitize(&e.name)
        ));
        let signed = e.values.iter().any(|v| v.value < 0);
        let (int, size) = enum_int(e.underlying_size, signed);
        if seen_enum.insert(e.path.as_str()) && !table.names.contains_key(&e.path) {
            table.names.insert(
                e.path.clone(),
                NameEntry {
                    module: module.clone(),
                    name: rust_name.clone(),
                    kind: EntryKind::Enum,
                },
            );
            table.enum_ints.insert(e.path.clone(), (int, size));
        }
        enums.push(plan_enum(
            e,
            module,
            rust_name,
            int,
            size,
            signed,
            &mut degraded,
        ));
    }

    // Struct order: dependencies first.
    let order = dependency_order(&defs, &by_path);
    let mut planned: Vec<Option<PlannedStruct>> = (0..defs.len()).map(|_| None).collect();
    let mut fields_total = 0usize;
    for &i in &order {
        let Some(&def) = defs.get(i) else { continue };
        let rust_name = struct_names.get(i).cloned().unwrap_or_default();
        let module = table
            .names
            .get(&def.path)
            .filter(|_| by_path.get(def.path.as_str()) == Some(&i))
            .map(|e| e.module.clone())
            .or_else(|| module_of.get(def.package.as_str()).cloned())
            .unwrap_or_default();
        let ps = plan_struct(
            def,
            module,
            rust_name,
            &table,
            &defs,
            &by_path,
            &mut degraded,
        );
        fields_total += def.fields.len();
        if by_path.get(def.path.as_str()) == Some(&i) {
            table
                .layouts
                .insert(def.path.clone(), (ps.size, ps.plan.align));
        }
        if let Some(slot) = planned.get_mut(i) {
            *slot = Some(ps);
        }
    }

    // Functions, now that every layout is known.
    let mut functions = 0usize;
    let mut functions_skipped = 0usize;
    for (i, def) in defs.iter().enumerate() {
        let Some(Some(ps)) = planned.get_mut(i) else {
            continue;
        };
        let mut vals = Namespace::default();
        for f in &ps.fields {
            if let Some(c) = &f.const_name {
                vals.claim(c);
            }
            if let Some(b) = &f.bit {
                vals.claim(&b.getter);
                vals.claim(&b.setter);
            }
        }
        let ns = type_ns.entry(ps.module.clone()).or_default();
        let mut fns: Vec<&FunctionDef> = def.functions.iter().collect();
        fns.sort();
        for f in fns {
            match plan_function(ps, f, &table, &mut vals, ns) {
                Ok(pf) => {
                    functions += 1;
                    ps.functions.push(pf);
                }
                Err(note) => {
                    functions_skipped += 1;
                    degraded.push(Degradation {
                        owner: f.path.clone(),
                        member: f.name.clone(),
                        reason: DegradeReason::FunctionSkipped,
                        detail: note,
                    });
                }
            }
        }
    }

    let structs: Vec<PlannedStruct> = planned.into_iter().flatten().collect();

    // Module dependencies for the features.
    let mut modules: Vec<ModuleInfo> = packages
        .iter()
        .filter_map(|pkg| {
            module_of.get(pkg).map(|name| ModuleInfo {
                package: (*pkg).to_owned(),
                name: name.clone(),
                deps: BTreeSet::new(),
            })
        })
        .collect();
    for s in &structs {
        let refs = s
            .refs
            .iter()
            .chain(s.functions.iter().flat_map(|f| f.refs.iter()));
        if let Some(m) = modules.iter_mut().find(|m| m.name == s.module) {
            m.deps.extend(refs.filter(|r| **r != s.module).cloned());
        }
    }
    degraded.sort();
    Built {
        modules,
        structs,
        enums,
        fname,
        degraded,
        fields: fields_total,
        functions,
        functions_skipped,
    }
}

fn fits(value: i64, size: u32, signed: bool) -> bool {
    let bits = size * 8;
    if bits >= 64 {
        return signed || value >= 0;
    }
    if signed {
        let half = 1i64 << (bits - 1);
        (-half..half).contains(&value)
    } else {
        (0..(1i64 << bits)).contains(&value)
    }
}

fn plan_enum(
    e: &EnumDef,
    module: String,
    rust_name: String,
    int: &'static str,
    size: u32,
    signed: bool,
    degraded: &mut Vec<Degradation>,
) -> PlannedEnum {
    let mut ns = Namespace::with_reserved(&["name"]);
    let mut values: Vec<&ufathom_model::EnumValue> = e.values.iter().collect();
    values.sort_by(|a, b| a.value.cmp(&b.value).then_with(|| a.name.cmp(&b.name)));
    let mut consts = Vec::new();
    let mut names = Vec::new();
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    for v in values {
        if !fits(v.value, size, signed) {
            degraded.push(Degradation {
                owner: e.path.clone(),
                member: v.name.clone(),
                reason: DegradeReason::EnumeratorOutOfRange,
                detail: format!("value {} does not fit {int}", v.value),
            });
            continue;
        }
        let const_name = ns.claim(&names::plain(&v.name));
        if seen.insert(v.value) {
            names.push((v.value, v.name.clone()));
        }
        consts.push((const_name, v.value));
    }
    PlannedEnum {
        path: e.path.clone(),
        module,
        rust_name,
        int,
        consts,
        names,
    }
}

/// Struct indices with parents and by-value members before their users. Types
/// in a cycle come last, in path order.
fn dependency_order(defs: &[&StructDef], by_path: &BTreeMap<&str, usize>) -> Vec<usize> {
    let deps_of = |i: usize| -> BTreeSet<usize> {
        let mut deps = BTreeSet::new();
        let Some(def) = defs.get(i) else { return deps };
        if let Some(&p) = def.super_path.as_deref().and_then(|p| by_path.get(p)) {
            deps.insert(p);
        }
        for f in &def.fields {
            if let PropertyType::Struct { path } = &f.ty {
                if let Some(&p) = by_path.get(path.as_str()) {
                    deps.insert(p);
                }
            }
        }
        deps.remove(&i);
        deps
    };
    let mut indegree: Vec<usize> = vec![0; defs.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); defs.len()];
    for i in 0..defs.len() {
        // Edges go to the first definition of a path only; a duplicate is
        // planned like any other type.
        let deps = deps_of(i);
        if let Some(d) = indegree.get_mut(i) {
            *d = deps.len();
        }
        for dep in deps {
            if let Some(list) = dependents.get_mut(dep) {
                list.push(i);
            }
        }
    }
    let mut ready: BTreeSet<usize> = (0..defs.len())
        .filter(|&i| indegree.get(i) == Some(&0))
        .collect();
    let mut order = Vec::with_capacity(defs.len());
    let mut done = vec![false; defs.len()];
    while let Some(i) = ready.pop_first() {
        order.push(i);
        if let Some(d) = done.get_mut(i) {
            *d = true;
        }
        for &next in dependents.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(d) = indegree.get_mut(next) {
                *d = d.saturating_sub(1);
                if *d == 0 {
                    ready.insert(next);
                }
            }
        }
    }
    order.extend((0..defs.len()).filter(|&i| done.get(i) == Some(&false)));
    order
}

/// A field turned into planner input plus the resolver's complaint, if any.
struct Classified {
    field: PlanField,
    resolved: Option<Resolved>,
    complaint: Option<(DegradeReason, String)>,
}

fn classify(f: &FieldDef, table: &TypeTable) -> Classified {
    let count = f.array_dim.max(1);
    let span = f.element_size.saturating_mul(count);
    let opaque = |reason: DegradeReason, detail: String| Classified {
        field: PlanField {
            offset: f.offset,
            kind: FieldKind::Opaque { span, reason },
        },
        resolved: None,
        complaint: Some((reason, detail)),
    };
    if let Some(bits) = f.ty.bool_bits() {
        if bits.field_mask != 0xFF {
            if count != 1 {
                return opaque(
                    DegradeReason::InvalidBitfield,
                    format!("a bit-field bool with {count} elements"),
                );
            }
            return Classified {
                field: PlanField {
                    offset: f.offset,
                    kind: FieldKind::Bit {
                        byte_offset: bits.byte_offset,
                        mask: bits.byte_mask,
                        field_size: bits.field_size,
                    },
                },
                resolved: None,
                complaint: None,
            };
        }
    }
    match table.resolve(&f.ty) {
        Ok(r) if r.size == f.element_size => Classified {
            field: PlanField {
                offset: f.offset,
                kind: FieldKind::Typed {
                    size: r.size,
                    align: r.align,
                    count,
                },
            },
            resolved: Some(r),
            complaint: None,
        },
        Ok(r) => opaque(
            DegradeReason::SizeMismatch,
            format!(
                "the dump says {} bytes, {} is {} bytes",
                f.element_size, r.rust, r.size
            ),
        ),
        Err(u) => opaque(u.reason, u.detail),
    }
}

/// Fills the planned fields of one plan: names for fields, constants and bits.
fn name_fields(
    fields: &[&FieldDef],
    classified: Vec<Classified>,
    plan: &Plan,
    fields_ns: &mut Namespace,
    vals: &mut Namespace,
    with_consts: bool,
) -> Vec<PlannedField> {
    let omitted: BTreeMap<usize, DegradeReason> =
        plan.omitted.iter().map(|o| (o.field, o.reason)).collect();
    let mut out: Vec<PlannedField> = fields
        .iter()
        .zip(classified)
        .map(|(f, c)| PlannedField {
            name: f.name.clone(),
            offset: f.offset,
            element_size: f.element_size,
            count: f.array_dim.max(1),
            ty_display: f.ty.to_string(),
            resolved: c.resolved,
            rust_field: None,
            const_name: None,
            bit: None,
            is_bit: matches!(c.field.kind, FieldKind::Bit { .. }),
            bit_raw: match c.field.kind {
                FieldKind::Bit {
                    byte_offset, mask, ..
                } => Some((byte_offset, mask)),
                _ => None,
            },
            is_bytes: false,
            note: c
                .complaint
                .map(|(reason, detail)| format!("{reason}: {detail}")),
        })
        .collect();
    for n in &plan.notes {
        if let Some(p) = n.field.and_then(|i| out.get_mut(i)) {
            p.note
                .get_or_insert_with(|| format!("{}: {}", n.reason, n.detail));
        }
    }

    // Rust fields in memory order.
    let mut storage: BTreeMap<u32, String> = BTreeMap::new();
    for item in &plan.items {
        match item {
            Item::Typed { field, .. } | Item::Bytes { field, .. } => {
                if let Some(p) = out.get_mut(*field) {
                    p.rust_field = Some(fields_ns.claim(&names::snake(&p.name)));
                    p.is_bytes = matches!(item, Item::Bytes { .. });
                }
            }
            Item::Bits { offset, .. } => {
                storage.insert(*offset, fields_ns.claim(&format!("_bits_{offset:#06x}")));
            }
            Item::Pad { .. } => {}
        }
    }
    // Constants and bit accessors in dump order, so names are stable.
    for (i, p) in out.iter_mut().enumerate() {
        if omitted.get(&i) == Some(&DegradeReason::OutOfBounds) {
            continue;
        }
        if with_consts {
            p.const_name = Some(vals.claim(&names::shouty(&p.name)));
        }
    }
    for item in &plan.items {
        if let Item::Bits { offset, bits, .. } = item {
            let storage_field = storage.get(offset).cloned().unwrap_or_default();
            for b in bits {
                if let Some(p) = out.get_mut(b.field) {
                    let snake = names::snake(&p.name);
                    p.bit = Some(BitAccess {
                        getter: vals.claim(&snake),
                        setter: vals.claim(&format!("set_{snake}")),
                        storage_byte: b.byte,
                        storage_field: storage_field.clone(),
                        mask: b.mask,
                        abs_offset: offset + b.byte,
                    });
                }
            }
        }
    }
    out
}

fn collect_notes(
    owner: &str,
    fields: &[&FieldDef],
    classified: &[Classified],
    plan: &Plan,
    degraded: &mut Vec<Degradation>,
) {
    let omitted: BTreeSet<usize> = plan.omitted.iter().map(|o| o.field).collect();
    for (i, c) in classified.iter().enumerate() {
        if let (Some((reason, detail)), false) = (&c.complaint, omitted.contains(&i)) {
            degraded.push(Degradation {
                owner: owner.to_owned(),
                member: fields.get(i).map(|f| f.name.clone()).unwrap_or_default(),
                reason: *reason,
                detail: detail.clone(),
            });
        }
    }
    for Note {
        field,
        reason,
        detail,
    } in &plan.notes
    {
        degraded.push(Degradation {
            owner: owner.to_owned(),
            member: match field {
                Some(i) => fields.get(*i).map(|f| f.name.clone()).unwrap_or_default(),
                None => "<base>".to_owned(),
            },
            reason: *reason,
            detail: detail.clone(),
        });
    }
}

fn plan_struct(
    def: &StructDef,
    module: String,
    rust_name: String,
    table: &TypeTable,
    defs: &[&StructDef],
    by_path: &BTreeMap<&str, usize>,
    degraded: &mut Vec<Degradation>,
) -> PlannedStruct {
    let mut sorted: Vec<&FieldDef> = def.fields.iter().collect();
    sorted.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.cmp(b))
    });
    let classified: Vec<Classified> = sorted.iter().map(|f| classify(f, table)).collect();

    // The parent, when it is generated and its layout is known.
    let parent = def
        .super_path
        .as_deref()
        .filter(|p| *p != def.path)
        .and_then(|p| table.layouts.get(p).map(|l| (p, *l)))
        .filter(|(p, _)| {
            table
                .names
                .get(*p)
                .is_some_and(|e| e.kind != EntryKind::Enum)
        });
    let input = PlanInput {
        size: def.size,
        align_hint: def.alignment,
        base: parent.map(|(_, (size, align))| BaseInfo { size, align }),
        fields: classified.iter().map(|c| c.field.clone()).collect(),
    };
    let planned = plan(&input);
    let mut notes_plan = planned.clone();
    if def.super_path.is_some() && parent.is_none() {
        notes_plan.notes.push(Note {
            field: None,
            reason: DegradeReason::BaseNotEmbedded,
            detail: format!(
                "parent {} is not in the dump or has no layout",
                def.super_path.as_deref().unwrap_or_default()
            ),
        });
    }
    collect_notes(&def.path, &sorted, &classified, &notes_plan, degraded);

    let mut fields_ns = Namespace::default();
    if planned.base == BaseMode::Embedded {
        fields_ns.claim("base");
    }
    let mut vals = Namespace::default();
    let fields = name_fields(
        &sorted,
        classified,
        &planned,
        &mut fields_ns,
        &mut vals,
        true,
    );

    // Ancestors, nearest first, while they are generated.
    let mut ancestors = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    seen.insert(def.path.as_str());
    let mut cur = def.super_path.as_deref();
    while let Some(path) = cur {
        if !seen.insert(path) {
            break;
        }
        let Some(&idx) = by_path.get(path) else { break };
        let Some(&anc) = defs.get(idx) else { break };
        let Some(entry) = table.names.get(path) else {
            break;
        };
        if anc.size <= def.size {
            ancestors.push(entry.rust_path());
        }
        cur = anc.super_path.as_deref();
    }
    let base_path = parent
        .and_then(|(p, _)| table.names.get(p))
        .map(NameEntry::rust_path);

    let mut refs = BTreeSet::new();
    for f in &fields {
        if f.rust_field.is_some() {
            if let Some(r) = &f.resolved {
                refs.extend(r.refs.iter().cloned());
            }
        }
    }
    for a in &ancestors {
        if let Some(m) = a.strip_prefix("crate::").and_then(|r| r.split("::").next()) {
            refs.insert(m.to_owned());
        }
    }

    PlannedStruct {
        path: def.path.clone(),
        module,
        rust_name,
        is_class: def.kind == StructKind::Class,
        size: def.size,
        plan: planned,
        fields,
        base_path,
        ancestors,
        functions: Vec::new(),
        refs,
    }
}

fn plan_function(
    ps: &PlannedStruct,
    f: &FunctionDef,
    table: &TypeTable,
    vals: &mut Namespace,
    type_ns: &mut Namespace,
) -> Result<PlannedFunction, String> {
    // Params in declared order: the planner returns indices into this order.
    let params: Vec<&FieldDef> = f.params.iter().collect();
    let classified: Vec<Classified> = params.iter().map(|p| classify(p, table)).collect();
    let input = PlanInput {
        size: f.params_size,
        align_hint: PARAM_ALIGN,
        base: None,
        fields: classified.iter().map(|c| c.field.clone()).collect(),
    };
    let planned = plan(&input);
    if let Some(o) = planned.omitted.first() {
        let name = params.get(o.field).map(|p| p.name.as_str()).unwrap_or("?");
        return Err(format!(
            "parameter {name} could not be placed in the {:#x} byte parameter block ({})",
            f.params_size, o.reason
        ));
    }
    let mut fields_ns = Namespace::default();
    let mut struct_vals = Namespace::default();
    let fields = name_fields(
        &params,
        classified,
        &planned,
        &mut fields_ns,
        &mut struct_vals,
        false,
    );
    let wrapper = vals.claim(&names::snake(&f.name));
    let params_struct = type_ns.claim(&format!(
        "{}_{}_Params",
        ps.rust_name,
        names::sanitize(&f.name)
    ));

    let mut arg_ns = Namespace::with_reserved(&["this", "exec", "params"]);
    let mut inputs = Vec::new();
    let mut outs = Vec::new();
    let mut ret = None;
    let mut refs = BTreeSet::new();
    for (i, p) in params.iter().enumerate() {
        let Some(pf) = fields.get(i) else { continue };
        let binding = |arg: String| -> ParamBinding {
            let is_ubool = pf
                .resolved
                .as_ref()
                .is_some_and(|r| r.rust == "rt::UBool" && pf.count == 1);
            let api_type = if pf.is_bit || is_ubool {
                "bool".to_owned()
            } else if let Some(r) = pf.resolved.as_ref() {
                if pf.count > 1 {
                    format!("[{}; {}]", r.rust, pf.count)
                } else {
                    r.rust.clone()
                }
            } else {
                format!("[u8; {}]", p.element_size.saturating_mul(pf.count))
            };
            ParamBinding {
                field: i,
                arg,
                api_type,
                is_ubool,
            }
        };
        if let Some(r) = &pf.resolved {
            refs.extend(r.refs.iter().cloned());
        }
        if p.flags & CPF_RETURN_PARM != 0 {
            if ret.is_none() {
                ret = Some(binding("return_value".to_owned()));
            }
        } else if p.flags & CPF_OUT_PARM != 0 && p.flags & CPF_CONST_PARM == 0 {
            outs.push(binding(arg_ns.claim(&names::snake(&p.name))));
        } else {
            inputs.push(binding(arg_ns.claim(&names::snake(&p.name))));
        }
    }
    Ok(PlannedFunction {
        path: f.path.clone(),
        wrapper,
        params_struct,
        size: f.params_size,
        plan: planned,
        fields,
        inputs,
        ret,
        outs,
        refs,
    })
}
