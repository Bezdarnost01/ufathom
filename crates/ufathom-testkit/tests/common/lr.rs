//! A second test-only reader, driven by `Fixture::layout`. It checks that
//! what the writer produced is readable under the layout it claims, for any
//! profile including shuffled ones. (The calibration tests use raw offsets
//! instead, for the measured builds.)

use ufathom_mem::{MemoryReader, MemoryReaderExt};
use ufathom_model::layout::{
    Location, NameTableLayout, ObjectArrayKind, PropertySystem, UEnumLayout,
};
use ufathom_model::{PropertyType, StructDef};
use ufathom_testkit::Fixture;

pub struct Lr<'a> {
    pub fx: &'a Fixture,
}

impl<'a> Lr<'a> {
    pub fn new(fx: &'a Fixture) -> Self {
        Self { fx }
    }

    pub fn u8(&self, addr: u64) -> Option<u8> {
        self.fx.memory.read_u8(addr).ok()
    }

    pub fn u16(&self, addr: u64) -> Option<u16> {
        self.fx.memory.read_u16(addr).ok()
    }

    pub fn u32(&self, addr: u64) -> Option<u32> {
        self.fx.memory.read_u32(addr).ok()
    }

    pub fn u64(&self, addr: u64) -> Option<u64> {
        self.fx.memory.read_u64(addr).ok()
    }

    pub fn loc(&self, location: &Location) -> u64 {
        match location {
            Location::Rva { rva, .. } => self.fx.at_rva(*rva),
            Location::Absolute(addr) => *addr,
        }
    }

    pub fn array(&self) -> u64 {
        self.loc(&self.fx.layout.objects.location)
    }

    pub fn num_elements(&self) -> u64 {
        let layout = &self.fx.layout.objects;
        u64::from(
            self.u32(self.array() + u64::from(layout.num_elements))
                .unwrap_or(0),
        )
    }

    pub fn max_elements(&self) -> u64 {
        let layout = &self.fx.layout.objects;
        u64::from(
            self.u32(self.array() + u64::from(layout.max_elements))
                .unwrap_or(0),
        )
    }

    /// The address of the item in slot `i`.
    pub fn item(&self, i: u64) -> Option<u64> {
        let layout = &self.fx.layout.objects;
        let table = self.u64(self.array() + u64::from(layout.objects))?;
        let size = u64::from(layout.item_size);
        match layout.kind {
            ObjectArrayKind::Fixed => Some(table + i * size),
            ObjectArrayKind::Chunked { elements_per_chunk } => {
                let per = u64::from(elements_per_chunk);
                let chunk = self.u64(table + (i / per) * 8)?;
                Some(chunk + (i % per) * size)
            }
        }
    }

    /// The object in slot `i`: `None` for a free slot or an unreadable item.
    pub fn object(&self, i: u64) -> Option<u64> {
        let layout = &self.fx.layout.objects;
        let item = self.item(i)?;
        let ptr = self.u64(item + u64::from(layout.item_object))?;
        (ptr != 0).then_some(ptr)
    }

    /// The slot index stored in an object.
    pub fn stored_index(&self, object: u64) -> Option<u32> {
        self.u32(object + u64::from(self.fx.layout.uobject.index))
    }

    /// Decodes the name with the given stored index.
    pub fn name_at(&self, index: u32) -> Option<String> {
        match &self.fx.layout.names {
            NameTableLayout::Pool(pool) => {
                let block = u64::from(index >> pool.block_offset_bits);
                let mask = (1u32 << pool.block_offset_bits) - 1;
                let offset = u64::from(index & mask) * u64::from(pool.stride);
                let base =
                    self.u64(self.loc(&pool.location) + u64::from(pool.blocks) + block * 8)?;
                let entry = base + offset;
                let header = u64::from(self.u16(entry + u64::from(pool.entry.header_offset))?);
                let len = usize::try_from(header >> pool.entry.len_shift).ok()?;
                let wide = header & u64::from(pool.entry.wide_mask) != 0;
                self.string(entry + u64::from(pool.entry.string_offset), len, wide)
            }
            NameTableLayout::Array(array) => {
                let slot = self.loc(&array.location);
                let object = if array.indirect {
                    self.u64(slot)?
                } else {
                    slot
                };
                let per = u64::from(array.elements_per_chunk);
                let chunk = self.u64(object + (u64::from(index) / per) * 8)?;
                let entry = self.u64(chunk + (u64::from(index) % per) * 8)?;
                let stored = self.u32(entry + u64::from(array.entry.index_offset))?;
                if stored >> 1 != index {
                    return None;
                }
                let wide = stored & array.entry.wide_mask != 0;
                let mut len = 0usize;
                let start = entry + u64::from(array.entry.string_offset);
                loop {
                    let unit = if wide {
                        u64::from(self.u16(start + len as u64 * 2)?)
                    } else {
                        u64::from(self.u8(start + len as u64)?)
                    };
                    if unit == 0 || len > 2048 {
                        break;
                    }
                    len += 1;
                }
                self.string(start, len, wide)
            }
        }
    }

    fn string(&self, addr: u64, len: usize, wide: bool) -> Option<String> {
        if wide {
            let mut units = Vec::new();
            for i in 0..len {
                units.push(self.u16(addr + i as u64 * 2)?);
            }
            String::from_utf16(&units).ok()
        } else {
            let mut bytes = vec![0u8; len];
            self.fx.memory.read_exact(addr, &mut bytes).ok()?;
            String::from_utf8(bytes).ok()
        }
    }

    /// Decodes an `FName` at `addr`, applying the instance number.
    pub fn fname(&self, addr: u64) -> Option<String> {
        let layout = &self.fx.layout.fname;
        let index = self.u32(addr + u64::from(layout.comparison_index))?;
        let base = self.name_at(index)?;
        let number = match layout.number {
            Some(offset) => self.u32(addr + u64::from(offset))?,
            None => 0,
        };
        Some(if number == 0 {
            base
        } else {
            format!("{base}_{}", number - 1)
        })
    }

    pub fn name_of(&self, object: u64) -> Option<String> {
        self.fname(object + u64::from(self.fx.layout.uobject.name))
    }

    pub fn class_of(&self, object: u64) -> Option<u64> {
        self.u64(object + u64::from(self.fx.layout.uobject.class))
    }

    pub fn class_name_of(&self, object: u64) -> Option<String> {
        self.name_of(self.class_of(object)?)
    }

    pub fn chain(&self, head: u64, next: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut at = head;
        while at != 0 && out.len() < 300 && !out.contains(&at) {
            out.push(at);
            match self.u64(at + next) {
                Some(n) => at = n,
                None => break,
            }
        }
        out
    }

    /// The property chain of a struct or function: address and name.
    pub fn properties(&self, owner: u64) -> Vec<(u64, String)> {
        let l = &self.fx.layout;
        match (&l.properties.system, l.ustruct.child_properties) {
            (PropertySystem::Modern { ffield, .. }, Some(child_properties)) => {
                let head = self.u64(owner + u64::from(child_properties)).unwrap_or(0);
                self.chain(head, u64::from(ffield.next))
                    .into_iter()
                    .filter_map(|f| Some((f, self.fname(f + u64::from(ffield.name))?)))
                    .collect()
            }
            _ => {
                let head = self.u64(owner + u64::from(l.ustruct.children)).unwrap_or(0);
                self.chain(head, u64::from(l.ufield.next))
                    .into_iter()
                    .filter(|&m| self.class_name_of(m).as_deref() != Some("Function"))
                    .filter_map(|m| Some((m, self.name_of(m)?)))
                    .collect()
            }
        }
    }

    /// The `UFunction` members of a struct.
    pub fn functions(&self, owner: u64) -> Vec<(u64, String)> {
        let l = &self.fx.layout;
        let head = self.u64(owner + u64::from(l.ustruct.children)).unwrap_or(0);
        self.chain(head, u64::from(l.ufield.next))
            .into_iter()
            .filter(|&m| self.class_name_of(m).as_deref() == Some("Function"))
            .filter_map(|m| Some((m, self.name_of(m)?)))
            .collect()
    }

    /// The class name of a property.
    pub fn property_class_name(&self, property: u64) -> Option<String> {
        let l = &self.fx.layout;
        match &l.properties.system {
            PropertySystem::Modern {
                ffield,
                field_class,
            } => {
                let class = self.u64(property + u64::from(ffield.class))?;
                self.fname(class + u64::from(field_class.name))
            }
            PropertySystem::Legacy => self.class_name_of(property),
        }
    }

    /// Reads one enum's `(name, value)` pairs, names with the enum prefix.
    pub fn enum_values(&self, object: u64) -> Option<Vec<(String, i64)>> {
        let l = &self.fx.layout;
        let size = u64::from(l.fname.size);
        let mut out = Vec::new();
        match l.uenum {
            UEnumLayout::PairArray {
                offset,
                stride,
                value_offset,
                value_width,
            } => {
                let array = object + u64::from(offset);
                let data = self.u64(array)?;
                let count = u64::from(self.u32(array + 8)?);
                for i in 0..count.min(256) {
                    let element = data + i * u64::from(stride);
                    let name = self.fname(element)?;
                    let at = element + u64::from(value_offset);
                    let value = match value_width {
                        1 => i64::from(self.u8(at)?),
                        _ => self.u64(at)? as i64,
                    };
                    out.push((name, value));
                }
            }
            UEnumLayout::SplitArrays {
                names,
                values,
                count,
            } => {
                let names_ptr = self.u64(object + u64::from(names))? & !7;
                let values_ptr = self.u64(object + u64::from(values))? & !7;
                let count = u64::from(self.u32(object + u64::from(count))?);
                for i in 0..count.min(256) {
                    let name = self.fname(names_ptr + i * size)?;
                    let value = self.u64(values_ptr + i * 8)? as i64;
                    out.push((name, value));
                }
            }
        }
        Some(out)
    }
}

/// The property class name expected for simple types; `None` for the rest.
fn expected_class(ty: &PropertyType, fx: &Fixture) -> Option<String> {
    let name = match ty {
        PropertyType::Int32 => "IntProperty",
        PropertyType::Float => "FloatProperty",
        PropertyType::Double => "DoubleProperty",
        PropertyType::UInt8 | PropertyType::Byte { .. } => "ByteProperty",
        PropertyType::Name => "NameProperty",
        PropertyType::Str => "StrProperty",
        PropertyType::Object { .. } => "ObjectProperty",
        PropertyType::Struct { .. } => "StructProperty",
        PropertyType::Array { .. } => "ArrayProperty",
        PropertyType::Bool { .. } => "BoolProperty",
        PropertyType::MulticastInlineDelegate { .. } => {
            return Some(fx.profile.shape.multicast_inline_class.clone());
        }
        _ => return None,
    };
    Some(name.to_owned())
}

impl Lr<'_> {
    /// Walks every struct, function and enum of the truth and compares them to
    /// what is in memory. Returns the number of members checked.
    pub fn check_reflection(&self) -> usize {
        let fx = self.fx;
        let mut checked = 0;
        for def in &fx.truth.structs {
            let object = fx
                .object(&def.path)
                .unwrap_or_else(|| panic!("{} is in the object array", def.path))
                .address;
            self.check_struct(def, object);
            checked += self.check_members(def, object);
            checked += self.check_functions(def, object);
        }
        for def in &fx.truth.enums {
            let object = fx.object(&def.path).expect("enum object").address;
            let values = self.enum_values(object).expect("enum values readable");
            let prefix = format!("{}::", def.name);
            let mut got: Vec<(String, i64)> = values
                .into_iter()
                .map(|(name, value)| {
                    (
                        name.strip_prefix(&prefix).unwrap_or(&name).to_owned(),
                        value,
                    )
                })
                .collect();
            got.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            let want: Vec<(String, i64)> = def
                .values
                .iter()
                .map(|v| (v.name.clone(), v.value))
                .collect();
            assert_eq!(got, want, "enum {}", def.path);
        }
        checked
    }

    fn check_struct(&self, def: &StructDef, object: u64) {
        let fx = self.fx;
        let st = &fx.layout.ustruct;
        assert_eq!(
            self.u32(object + u64::from(st.properties_size)),
            Some(def.size),
            "{}",
            def.path
        );
        let align = if st.min_alignment_width == 2 {
            self.u16(object + u64::from(st.min_alignment))
                .map(u32::from)
        } else {
            self.u32(object + u64::from(st.min_alignment))
        };
        assert_eq!(align, Some(def.alignment), "{} alignment", def.path);
        let super_addr = self.u64(object + u64::from(st.super_struct)).unwrap();
        match &def.super_path {
            Some(path) => assert_eq!(
                super_addr,
                fx.object(path).unwrap().address,
                "{} super",
                def.path
            ),
            None => assert_eq!(super_addr, 0, "{} has no super", def.path),
        }
    }

    fn check_members(&self, def: &StructDef, object: u64) -> usize {
        let fx = self.fx;
        let props = &fx.layout.properties;
        let found = self.properties(object);
        let mut got: Vec<(String, u32, u32, u32, u64)> = found
            .iter()
            .map(|(addr, name)| {
                (
                    name.clone(),
                    self.u32(addr + u64::from(props.offset_internal)).unwrap(),
                    self.u32(addr + u64::from(props.element_size)).unwrap(),
                    self.u32(addr + u64::from(props.array_dim)).unwrap(),
                    self.u64(addr + u64::from(props.flags)).unwrap(),
                )
            })
            .collect();
        got.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let want: Vec<(String, u32, u32, u32, u64)> = def
            .fields
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    f.offset,
                    f.element_size,
                    f.array_dim,
                    f.flags,
                )
            })
            .collect();
        assert_eq!(got, want, "members of {}", def.path);
        // Types: class names and typed payloads, matched by name.
        for field in &def.fields {
            let Some((addr, _)) = found.iter().find(|(_, n)| *n == field.name) else {
                continue;
            };
            self.check_type(&def.path, &field.name, *addr, &field.ty);
        }
        def.fields.len()
    }

    fn check_type(&self, owner: &str, name: &str, addr: u64, ty: &PropertyType) {
        let fx = self.fx;
        let typed = &fx.layout.properties.typed;
        let context = format!("{owner}:{name}");
        if let Some(expected) = expected_class(ty, fx) {
            assert_eq!(self.property_class_name(addr), Some(expected), "{context}");
        }
        let ptr = |offset: Option<u32>| {
            self.u64(addr + u64::from(offset.expect("offset known")))
                .unwrap()
        };
        match ty {
            PropertyType::Bool {
                byte_offset,
                byte_mask,
                field_mask,
                field_size,
            } => {
                let b = typed.bool_property.as_ref().unwrap();
                let at = |o: u32| self.u8(addr + u64::from(o)).unwrap();
                assert_eq!(
                    [
                        at(b.field_size),
                        at(b.byte_offset),
                        at(b.byte_mask),
                        at(b.field_mask)
                    ],
                    [*field_size, *byte_offset, *byte_mask, *field_mask],
                    "{context}"
                );
            }
            PropertyType::Object { class } => {
                let target = ptr(typed.object_class);
                assert_eq!(target, fx.object(class).unwrap().address, "{context}");
            }
            PropertyType::Struct { path } => {
                assert_eq!(
                    ptr(typed.script_struct),
                    fx.object(path).unwrap().address,
                    "{context}"
                );
            }
            PropertyType::Byte { path } => {
                assert_eq!(
                    ptr(typed.byte_enum),
                    fx.object(path).unwrap().address,
                    "{context}"
                );
            }
            PropertyType::Enum { path, .. } => {
                assert_eq!(
                    ptr(typed.enum_type),
                    fx.object(path).unwrap().address,
                    "{context}"
                );
            }
            PropertyType::Array { inner } => {
                let inner_addr = ptr(typed.array_inner);
                if let Some(expected) = expected_class(inner, fx) {
                    assert_eq!(
                        self.property_class_name(inner_addr),
                        Some(expected),
                        "{context} inner"
                    );
                }
            }
            PropertyType::Map { key, value } => {
                for (offset, ty) in [(typed.map_key, key), (typed.map_value, value)] {
                    let inner_addr = ptr(offset);
                    if let Some(expected) = expected_class(ty, fx) {
                        assert_eq!(
                            self.property_class_name(inner_addr),
                            Some(expected),
                            "{context} map"
                        );
                    }
                }
            }
            PropertyType::Delegate { signature }
            | PropertyType::MulticastInlineDelegate { signature } => {
                assert_eq!(
                    ptr(typed.delegate_signature),
                    fx.object(signature).unwrap().address,
                    "{context}"
                );
            }
            PropertyType::Class { meta, .. } | PropertyType::SoftClass { meta } => {
                assert_eq!(
                    ptr(typed.class_meta),
                    fx.object(meta).unwrap().address,
                    "{context}"
                );
            }
            _ => {}
        }
    }

    fn check_functions(&self, def: &StructDef, object: u64) -> usize {
        let fx = self.fx;
        let l = &fx.layout;
        let functions = self.functions(object);
        let mut names: Vec<&str> = functions.iter().map(|(_, n)| n.as_str()).collect();
        names.sort_unstable();
        let mut want: Vec<&str> = def.functions.iter().map(|f| f.name.as_str()).collect();
        want.sort_unstable();
        assert_eq!(names, want, "functions of {}", def.path);
        for function in &def.functions {
            let (addr, _) = functions.iter().find(|(_, n)| *n == function.name).unwrap();
            assert_eq!(
                self.u32(addr + u64::from(l.ufunction.flags)),
                Some(function.flags),
                "{}",
                function.path
            );
            assert_eq!(
                self.u32(addr + u64::from(l.ustruct.properties_size)),
                Some(function.params_size),
                "{}",
                function.path
            );
            if let Some(native) = l.ufunction.native_func {
                let func = self.u64(addr + u64::from(native)).unwrap();
                match function.native_rva {
                    Some(rva) => assert_eq!(func, fx.at_rva(rva), "{} Func", function.path),
                    None => assert_eq!(func, 0, "{} has no native code", function.path),
                }
            }
            // Parameters sorted by offset are in declaration order.
            let mut params = self.properties(*addr);
            params.sort_by_key(|(a, _)| {
                self.u32(a + u64::from(l.properties.offset_internal))
                    .unwrap()
            });
            let got: Vec<(String, u32, u64)> = params
                .iter()
                .map(|(a, n)| {
                    (
                        n.clone(),
                        self.u32(a + u64::from(l.properties.offset_internal))
                            .unwrap(),
                        self.u64(a + u64::from(l.properties.flags)).unwrap(),
                    )
                })
                .collect();
            let want: Vec<(String, u32, u64)> = function
                .params
                .iter()
                .map(|p| (p.name.clone(), p.offset, p.flags))
                .collect();
            assert_eq!(got, want, "parameters of {}", function.path);
        }
        def.functions.len()
    }
}
