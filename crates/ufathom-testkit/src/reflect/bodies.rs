//! Object headers and the bodies of structs, classes, functions and enums.

use ufathom_model::StructKind;
use ufathom_model::flags::{CPF_RETURN_PARM, FUNC_NATIVE};
use ufathom_model::layout::{PropertySystem, UEnumLayout};

use crate::schema::PROPERTY_CLASSES;

use super::{ChainOrder, Kind, Writer};

/// `ReturnValueOffset` of a function with no return value.
const NO_RETURN_VALUE: u16 = 0xFFFF;
/// Tag bits stored in the low bits of the split enum array pointers. ASSUMED.
const ENUM_NAMES_TAG: u64 = 1;
const ENUM_VALUES_TAG: u64 = 2;
/// `ECppForm` values: regular, namespaced, enum class.
const CPP_FORM_REGULAR: u32 = 0;
const CPP_FORM_ENUM_CLASS: u32 = 2;

impl Writer<'_> {
    /// Address of a registered object. A missing path is a schema mistake.
    #[allow(
        clippy::expect_used,
        reason = "a dangling schema reference is a build-time mistake"
    )]
    pub(crate) fn addr_of(&self, path: &str) -> u64 {
        let index = self
            .by_path
            .get(path)
            .unwrap_or_else(|| panic!("schema refers to unknown object {path}"));
        self.objs
            .get(*index)
            .expect("registered object exists")
            .addr
    }

    /// Writes an `FName` at `addr` with the profile's shape.
    pub(crate) fn write_fname(&mut self, addr: u64, base: &str, number: u32) {
        let (index, stored_number) = self.names.fname_for(base, number);
        let fname = self.p.layout.fname.clone();
        self.mem
            .put_u32(addr + u64::from(fname.comparison_index), index);
        if let Some(display) = fname.display_index {
            self.mem.put_u32(addr + u64::from(display), index);
        }
        if let Some(number_offset) = fname.number {
            self.mem
                .put_u32(addr + u64::from(number_offset), stored_number);
        }
    }

    /// The parent of a vtable key, or `None` for a root.
    fn vtable_parent(&self, key: &str) -> Option<String> {
        if let Some(name) = key.strip_prefix("FField:") {
            let parent = PROPERTY_CLASSES
                .iter()
                .find(|&&(class, _)| class == name)
                .map(|&(_, parent)| parent)?;
            return Some(if parent == "Field" {
                "FField".to_owned()
            } else {
                format!("FField:{parent}")
            });
        }
        self.super_of.get(key).cloned()
    }

    /// The vtable of a C++ class, creating its ancestors first.
    pub(crate) fn vtable(&mut self, key: &str) -> u64 {
        if let Some(&addr) = self.vt_cache.get(key) {
            return addr;
        }
        let mut chain = vec![key.to_owned()];
        while let Some(parent) = chain.last().and_then(|last| self.vtable_parent(last)) {
            chain.push(parent);
        }
        chain.reverse();
        let mut previous: Option<String> = None;
        let mut addr = 0;
        for link in &chain {
            addr = self.code.vtable(self.mem, link, previous.as_deref());
            self.vt_cache.insert(link.clone(), addr);
            previous = Some(link.clone());
        }
        addr
    }

    /// The bit a class contributes to cast flags. ASSUMED: sequential bits, not
    /// the engine's real values.
    pub(crate) fn own_cast_bit(&self, name: &str) -> u64 {
        let short = crate::schema::canonical_class_name(name.rsplit('.').next().unwrap_or(name));
        match short {
            "Field" => 1,
            "Enum" => 1 << 1,
            "Struct" => 1 << 2,
            "ScriptStruct" => 1 << 3,
            "Class" => 1 << 4,
            "Function" => 1 << 5,
            "Package" => 1 << 6,
            "Actor" => 1 << 7,
            other => PROPERTY_CLASSES
                .iter()
                .position(|&(class, _)| class == other)
                .map_or(0, |k| 1u64 << (8 + k)),
        }
    }

    /// `ClassCastFlags` of a class: its own bit and those of its ancestors.
    fn class_cast_flags(&self, path: &str) -> u64 {
        let mut flags = 0;
        let mut current = Some(path.to_owned());
        let mut guard = 0;
        while let Some(class) = current {
            flags |= self.own_cast_bit(&class);
            current = self.super_of.get(&class).cloned();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        flags
    }

    pub(crate) fn field_offset(&self, struct_path: &str, field: &str) -> u32 {
        self.schema
            .structs
            .iter()
            .find(|def| def.path == struct_path)
            .and_then(|def| def.fields.iter().find(|f| f.name == field))
            .map_or(0, |f| f.offset)
    }

    /// Writes the `UObject` header of object `i`.
    pub(crate) fn write_header(&mut self, i: usize) {
        let Some(obj) = self.objs.get(i).cloned() else {
            return;
        };
        let layout = self.p.layout.uobject.clone();
        let vtable = self.vtable(&obj.class);
        let class = self.addr_of(&obj.class);
        let outer = obj.outer.as_deref().map_or(0, |path| self.addr_of(path));
        let a = obj.addr;
        self.mem.put_u64(a, vtable);
        self.mem.put_u32(a + u64::from(layout.flags), obj.flags);
        self.mem
            .put_u32(a + u64::from(layout.index), obj.stored_index);
        self.mem.put_u64(a + u64::from(layout.class), class);
        self.mem.put_u64(a + u64::from(layout.outer), outer);
        let name_at = a + u64::from(layout.name);
        if !self.write_mangled_fname(name_at, &obj.base_name, obj.number) {
            self.write_fname(name_at, &obj.base_name, obj.number);
        }
    }

    /// Writes an `FName` whose comparison entry is the lowercase twin of the
    /// display entry, while budget remains. Returns `false`, writing nothing,
    /// when the name does not qualify.
    fn write_mangled_fname(&mut self, addr: u64, base: &str, number: u32) -> bool {
        let fname = self.p.layout.fname.clone();
        let Some(display_offset) = fname.display_index else {
            return false;
        };
        let twin = base.to_ascii_lowercase();
        if self.mangled_left == 0 || number != 0 || twin == base || !base.is_ascii() {
            return false;
        }
        self.mangled_left -= 1;
        let display = self.names.intern(base);
        let comparison = self.names.intern(&twin);
        self.mem
            .put_u32(addr + u64::from(fname.comparison_index), comparison);
        self.mem.put_u32(addr + u64::from(display_offset), display);
        if let Some(number_offset) = fname.number {
            self.mem.put_u32(addr + u64::from(number_offset), 0);
        }
        true
    }

    /// Orders and links every member chain, recording the head of each list.
    pub(crate) fn compute_chains(&mut self) {
        let owners: Vec<(usize, String)> = self
            .objs
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o.kind, Kind::Struct(_) | Kind::Function { .. }))
            .map(|(i, o)| (i, o.path.clone()))
            .collect();
        let order = self.cfg.chain_order;
        let modern = self.p.is_modern();
        for (_, owner) in owners {
            let props: Vec<u64> = self
                .chains
                .get(&owner)
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter_map(|&n| self.nodes.get(n).map(|node| node.addr))
                        .collect()
                })
                .unwrap_or_default();
            let functions: Vec<u64> = self.function_addresses(&owner);
            let (children, child_props) = if modern {
                let mut fns = functions;
                let mut ps = props;
                if order == ChainOrder::Reversed {
                    fns.reverse();
                    ps.reverse();
                }
                (fns, ps)
            } else {
                let mut all = match order {
                    ChainOrder::FunctionsFirst => functions.into_iter().chain(props).collect(),
                    _ => props.into_iter().chain(functions).collect::<Vec<_>>(),
                };
                if order == ChainOrder::Reversed {
                    all.reverse();
                }
                (all, Vec::new())
            };
            let next = u64::from(self.p.layout.ufield.next);
            for pair in children.windows(2) {
                if let [a, b] = pair {
                    self.mem.put_u64(a + next, *b);
                }
            }
            if let PropertySystem::Modern { ffield, .. } = &self.p.layout.properties.system {
                let next = u64::from(ffield.next);
                for pair in child_props.windows(2) {
                    if let [a, b] = pair {
                        self.mem.put_u64(a + next, *b);
                    }
                }
            }
            let head = |list: &[u64]| list.first().copied().unwrap_or(0);
            self.heads
                .insert(owner, (head(&children), head(&child_props)));
        }
    }

    fn function_addresses(&self, owner: &str) -> Vec<u64> {
        self.schema
            .structs
            .iter()
            .find(|def| def.path == owner)
            .map(|def| {
                def.functions
                    .iter()
                    .map(|f| {
                        self.by_path
                            .get(&f.path)
                            .and_then(|&i| self.objs.get(i))
                            .map_or(0, |o| o.addr)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Writes the body of object `i`, by kind.
    pub(crate) fn write_body(&mut self, i: usize) {
        let Some(obj) = self.objs.get(i).cloned() else {
            return;
        };
        match obj.kind {
            Kind::Struct(def) => self.write_struct_body(&obj.path, obj.addr, def),
            Kind::Function { owner, func } => {
                self.write_function_body(&obj.path, obj.addr, owner, func)
            }
            Kind::Enum(def) => self.write_enum_body(obj.addr, def),
            Kind::Instance => self.write_instance_body(&obj.path, obj.addr),
            Kind::Cdo | Kind::Package | Kind::Property => {}
        }
        if let Kind::Cdo = obj.kind {
            // The default object of a class is an instance of that class: it
            // carries no body beyond its header here.
        }
    }

    fn write_struct_body(&mut self, path: &str, a: u64, def: usize) {
        let Some(def) = self.schema.structs.get(def).cloned() else {
            return;
        };
        let layout = self.p.layout.clone();
        let st = &layout.ustruct;
        let super_addr = def.super_path.as_deref().map_or(0, |s| self.addr_of(s));
        let (children, child_props) = self.heads.get(path).copied().unwrap_or((0, 0));
        self.mem.put_u64(a + u64::from(st.super_struct), super_addr);
        self.mem.put_u64(a + u64::from(st.children), children);
        if let Some(offset) = st.child_properties {
            self.mem.put_u64(a + u64::from(offset), child_props);
        }
        self.mem
            .put_u32(a + u64::from(st.properties_size), def.size);
        if st.min_alignment_width == 2 {
            self.mem.put_u16(
                a + u64::from(st.min_alignment),
                u16::try_from(def.alignment).unwrap_or(8),
            );
        } else {
            self.mem
                .put_u32(a + u64::from(st.min_alignment), def.alignment);
        }
        if def.kind == StructKind::Class {
            if let Some(offset) = layout.uclass.cast_flags {
                let flags = self.class_cast_flags(&def.path);
                self.mem.put_u64(a + u64::from(offset), flags);
            }
            if let Some(offset) = layout.uclass.default_object {
                let cdo = format!("{}.Default__{}", def.package, def.name);
                let target = self
                    .by_path
                    .get(&cdo)
                    .and_then(|&i| self.objs.get(i))
                    .map_or(0, |o| o.addr);
                self.mem.put_u64(a + u64::from(offset), target);
            }
        }
    }

    fn write_function_body(&mut self, path: &str, a: u64, owner: usize, func: usize) {
        let Some(function) = self
            .schema
            .structs
            .get(owner)
            .and_then(|def| def.functions.get(func))
            .cloned()
        else {
            return;
        };
        let layout = self.p.layout.clone();
        let st = &layout.ustruct;
        let (children, child_props) = self.heads.get(path).copied().unwrap_or((0, 0));
        self.mem.put_u64(a + u64::from(st.children), children);
        if let Some(offset) = st.child_properties {
            self.mem.put_u64(a + u64::from(offset), child_props);
        }
        self.mem
            .put_u32(a + u64::from(st.properties_size), function.params_size);
        if st.min_alignment_width == 2 {
            self.mem.put_u16(a + u64::from(st.min_alignment), 1);
        } else {
            self.mem.put_u32(a + u64::from(st.min_alignment), 1);
        }
        let flags = u64::from(layout.ufunction.flags);
        self.mem.put_u32(a + flags, function.flags);
        // NumParms, ParmsSize, ReturnValueOffset follow FunctionFlags.
        let block = flags + u64::from(self.p.shape.ufunction_parm_block);
        self.mem.put_u8(
            a + block,
            u8::try_from(function.params.len()).unwrap_or(u8::MAX),
        );
        self.mem.put_u16(
            a + block + 2,
            u16::try_from(function.params_size).unwrap_or(u16::MAX),
        );
        let return_offset = function
            .params
            .iter()
            .find(|p| p.flags & CPF_RETURN_PARM != 0)
            .map_or(NO_RETURN_VALUE, |p| {
                u16::try_from(p.offset).unwrap_or(NO_RETURN_VALUE)
            });
        self.mem.put_u16(a + block + 4, return_offset);
        if function.flags & FUNC_NATIVE != 0
            && let Some(native) = layout.ufunction.native_func
        {
            let thunk = self.code.native_thunk(self.mem);
            self.mem.put_u64(a + u64::from(native), thunk);
            self.native_rvas
                .insert(function.path.clone(), self.mem.addrs.rva(thunk));
        }
    }

    fn write_enum_body(&mut self, a: u64, def: usize) {
        let Some(def) = self.schema.enums.get(def).cloned() else {
            return;
        };
        let layout = self.p.layout.clone();
        let fname_size = layout.fname.size;
        // CppForm follows the names array (regular or enum class).
        let form = if def.values.iter().any(|v| v.name.starts_with('E')) {
            CPP_FORM_ENUM_CLASS
        } else {
            CPP_FORM_REGULAR
        };
        // Names are stored with the enum name as a prefix: `ENetRole::ROLE_None`.
        let full_names: Vec<(String, i64)> = def
            .values
            .iter()
            .map(|v| (format!("{}::{}", def.name, v.name), v.value))
            .collect();
        let count = full_names.len();
        match layout.uenum {
            UEnumLayout::PairArray {
                offset,
                stride,
                value_offset,
                value_width,
            } => {
                let bytes = usize::try_from(stride).unwrap_or(0x18) * count;
                let data = self.mem.heap.alloc(crate::space::Pool::Objects, bytes, 8);
                for (n, (name, value)) in full_names.iter().enumerate() {
                    let element = data + u64::from(stride) * n as u64;
                    self.write_fname(element, name, 0);
                    let raw = value.to_le_bytes();
                    let width = usize::try_from(value_width).unwrap_or(8).min(8);
                    self.mem.put(
                        element + u64::from(value_offset),
                        raw.get(..width).unwrap_or(&raw),
                    );
                }
                let array = a + u64::from(offset);
                self.mem.put_u64(array, data);
                self.mem
                    .put_u32(array + 8, u32::try_from(count).unwrap_or(0));
                self.mem
                    .put_u32(array + 12, u32::try_from(count).unwrap_or(0));
                self.mem.put_u32(array + 16, form);
            }
            UEnumLayout::SplitArrays {
                names,
                values,
                count: count_offset,
            } => {
                let name_bytes = usize::try_from(fname_size).unwrap_or(12) * count;
                let name_data = self
                    .mem
                    .heap
                    .alloc(crate::space::Pool::Objects, name_bytes, 8);
                let value_data = self
                    .mem
                    .heap
                    .alloc(crate::space::Pool::Objects, count * 8, 8);
                for (n, (name, value)) in full_names.iter().enumerate() {
                    self.write_fname(name_data + u64::from(fname_size) * n as u64, name, 0);
                    self.mem.put_u64(value_data + n as u64 * 8, *value as u64);
                }
                self.mem
                    .put_u64(a + u64::from(names), name_data | ENUM_NAMES_TAG);
                self.mem
                    .put_u64(a + u64::from(values), value_data | ENUM_VALUES_TAG);
                self.mem.put_u32(
                    a + u64::from(count_offset),
                    u32::try_from(count).unwrap_or(0),
                );
            }
        }
    }

    fn write_instance_body(&mut self, path: &str, a: u64) {
        let world = "/Game/Maps/Main.Main";
        if path == world {
            let level = self.addr_of(&format!("{world}:PersistentLevel"));
            let offset = self.field_offset("/Script/Engine.World", "PersistentLevel");
            self.mem.put_u64(a + u64::from(offset), level);
        } else if path == format!("{world}:PersistentLevel") {
            let world_addr = self.addr_of(world);
            let offset = self.field_offset("/Script/Engine.Level", "OwningWorld");
            self.mem.put_u64(a + u64::from(offset), world_addr);
        }
    }
}
