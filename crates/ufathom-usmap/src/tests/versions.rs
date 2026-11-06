//! Hand-built files for every format version.
//!
//! Each fixture is written byte by byte, independent of this crate's encoder,
//! to pin down what differs between versions.

use super::fixtures::{Bytes, file, header_prefix};
use crate::{Compression, Usmap, UsmapError, UsmapType, UsmapVersion};

const LONG_NAME_LEN: usize = 300;

/// Names of the logical file. Index 8 exists from version 2, where name
/// lengths grow to 16 bits and so can exceed 255.
fn names(version: u8) -> Vec<String> {
    let mut names: Vec<String> = [
        "EMode", "Off", "On", "Thing", "Base", "Value", "Items", "Mode",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    if version >= 2 {
        names.push("L".repeat(LONG_NAME_LEN));
    }
    names
}

/// The body of the logical file in `version`:
/// - enum `EMode` with `Off` and `On` (`On` is 7 when numbers are stored);
/// - struct `Thing : Base` with `Value: Int`, `Items[4]: Array<Struct Base>`,
///   `Mode: Enum<Byte> EMode`, and from version 2 a long-named
///   `Optional<Double>`.
fn body(version: u8) -> Vec<u8> {
    let mut out = Bytes::default().u32(u32::try_from(names(version).len()).unwrap_or(0));
    for name in names(version) {
        out = if version >= 2 {
            out.u16(u16::try_from(name.len()).unwrap_or(0))
        } else {
            out.u8(u8::try_from(name.len()).unwrap_or(0))
        };
        out = out.raw(name.as_bytes());
    }

    // Enums.
    out = out.u32(1).u32(0);
    out = if version >= 3 { out.u16(2) } else { out.u8(2) };
    if version >= 4 {
        out = out.i64(0).u32(1).i64(7).u32(2);
    } else {
        out = out.u32(1).u32(2);
    }

    // Structs.
    let slots: u16 = if version >= 2 { 7 } else { 6 };
    let listed: u16 = if version >= 2 { 4 } else { 3 };
    out = out.u32(1).u32(3).u32(4).u16(slots).u16(listed);
    out = out.u16(0).u8(1).u32(5).u8(2); // Value: Int
    out = out.u16(1).u8(4).u32(6).u8(8).u8(9).u32(4); // Items[4]: Array<Struct Base>
    out = out.u16(5).u8(1).u32(7).u8(26).u8(0).u32(0); // Mode: Enum<Byte> EMode
    if version >= 2 {
        out = out.u16(6).u8(1).u32(8).u8(28).u8(7); // long name: Optional<Double>
    }
    out.0
}

fn version_block(info: bool) -> Vec<u8> {
    if !info {
        return Bytes::default().u32(0).0;
    }
    Bytes::default()
        .u32(1)
        .i32(522)
        .i32(1008)
        .u32(1)
        .raw(&[0xAB; 16])
        .i32(9)
        .u32(4_242)
        .0
}

fn uncompressed(version: u8, info: bool) -> Vec<u8> {
    let body = body(version);
    let size = u32::try_from(body.len()).unwrap_or(0);
    file(
        header_prefix(version, &version_block(info), 0),
        size,
        size,
        &body,
    )
}

fn check_logical(usmap: &Usmap, version: u8) {
    assert_eq!(usmap.version.as_u8(), version);
    assert_eq!(usmap.compression, Compression::None);
    assert_eq!(usmap.names, names(version));
    assert_eq!(usmap.extension_bytes, 0);

    assert_eq!(usmap.enums.len(), 1);
    let mode = &usmap.enums[0];
    assert_eq!(mode.name, "EMode");
    let values: Vec<(&str, i64)> = mode
        .values
        .iter()
        .map(|value| (value.name.as_str(), value.value))
        .collect();
    if version >= 4 {
        assert_eq!(values, [("Off", 0), ("On", 7)]);
        assert!(usmap.has_explicit_enum_values());
    } else {
        // Before version 4 the number is the position.
        assert_eq!(values, [("Off", 0), ("On", 1)]);
        assert!(!usmap.has_explicit_enum_values());
    }

    assert_eq!(usmap.structs.len(), 1);
    let thing = &usmap.structs[0];
    assert_eq!(thing.name, "Thing");
    assert_eq!(thing.super_name.as_deref(), Some("Base"));
    assert_eq!(thing.property_count, if version >= 2 { 7 } else { 6 });

    let value = &thing.properties[0];
    assert_eq!(
        (value.name.as_str(), value.index, value.array_dim),
        ("Value", 0, 1)
    );
    assert_eq!(value.ty, UsmapType::Int32);

    let items = &thing.properties[1];
    assert_eq!(
        (items.name.as_str(), items.index, items.array_dim),
        ("Items", 1, 4)
    );
    assert_eq!(
        items.ty,
        UsmapType::Array {
            inner: Box::new(UsmapType::Struct {
                name: "Base".to_owned()
            })
        }
    );

    let mode_property = &thing.properties[2];
    assert_eq!(mode_property.index, 5);
    assert_eq!(
        mode_property.ty,
        UsmapType::Enum {
            inner: Box::new(UsmapType::Byte),
            name: "EMode".to_owned()
        }
    );

    if version >= 2 {
        let long = &thing.properties[3];
        assert_eq!(long.name.len(), LONG_NAME_LEN);
        assert_eq!(
            long.ty,
            UsmapType::Optional {
                inner: Box::new(UsmapType::Double)
            }
        );
        assert_eq!(thing.properties.len(), 4);
    } else {
        assert_eq!(thing.properties.len(), 3);
    }
}

#[test]
fn every_version_from_0_to_4_parses() {
    for version in 0..=4u8 {
        let usmap = Usmap::parse(&uncompressed(version, false))
            .unwrap_or_else(|error| panic!("version {version}: {error}"));
        assert_eq!(usmap.version_info, None);
        check_logical(&usmap, version);
    }
}

#[test]
fn version_block_is_read_from_version_1() {
    for version in 1..=4u8 {
        let usmap = Usmap::parse(&uncompressed(version, true)).expect("parses");
        let info = usmap.version_info.as_ref().expect("version info");
        assert_eq!((info.ue4, info.ue5, info.net_cl), (522, 1008, 4_242));
        assert_eq!(info.custom_versions.len(), 1);
        assert_eq!(info.custom_versions[0].guid, [0xAB; 16]);
        assert_eq!(info.custom_versions[0].version, 9);
        check_logical(&usmap, version);
    }
}

#[test]
fn version_0_has_no_version_block() {
    // The same bytes read as version 0 must not consume a flag: this is the
    // shape of a real 8.1.0 header, `c430 00 00 <sizes>`.
    let bytes = uncompressed(0, false);
    assert_eq!(&bytes[..4], &[0xC4, 0x30, 0x00, 0x00]);
    assert_eq!(
        Usmap::parse(&bytes).expect("parses").version,
        UsmapVersion::Initial
    );
}

#[test]
fn version_4_zstd_header_has_the_dumper_shape() {
    let body = body(4);
    let compressed = ruzstd::encoding::compress_to_vec(
        body.as_slice(),
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    let bytes = file(
        header_prefix(4, &version_block(false), 3),
        u32::try_from(compressed.len()).expect("size"),
        u32::try_from(body.len()).expect("size"),
        &compressed,
    );
    // Magic, version 4, 32-bit bHasVersionInfo = 0, compression 3 (zstd).
    assert_eq!(&bytes[..8], &[0xC4, 0x30, 0x04, 0, 0, 0, 0, 0x03]);

    let usmap = Usmap::parse(&bytes).expect("parses");
    assert_eq!(usmap.compression, Compression::Zstd);
    assert_eq!(usmap.names, names(4));
    assert_eq!(usmap.structs.len(), 1);
    assert_eq!(usmap.enums[0].values[1].value, 7);
}

#[test]
fn large_enums_need_a_16_bit_count_from_version_3() {
    let count = 300usize;
    let mut out = Bytes::default().u32(u32::try_from(count + 1).expect("count"));
    let mut all = vec!["EBig".to_owned()];
    all.extend((0..count).map(|index| format!("N{index}")));
    for name in &all {
        out = out
            .u16(u16::try_from(name.len()).expect("len"))
            .raw(name.as_bytes());
    }
    out = out.u32(1).u32(0).u16(u16::try_from(count).expect("count"));
    for index in 0..count {
        out = out
            .i64(i64::try_from(index).expect("index") * 2)
            .u32(u32::try_from(index + 1).expect("index"));
    }
    out = out.u32(0); // no structs
    let bytes = file(
        header_prefix(4, &version_block(false), 0),
        u32::try_from(out.0.len()).expect("size"),
        u32::try_from(out.0.len()).expect("size"),
        &out.0,
    );
    let usmap = Usmap::parse(&bytes).expect("parses");
    assert_eq!(usmap.enums[0].values.len(), count);
    assert_eq!(usmap.enums[0].values[299].value, 598);
}

#[test]
fn bytes_after_the_struct_list_are_counted_as_extensions() {
    let mut body = body(4);
    body.extend_from_slice(b"PPTH\x00\x00\x00\x00");
    let size = u32::try_from(body.len()).expect("size");
    let bytes = file(
        header_prefix(4, &version_block(false), 0),
        size,
        size,
        &body,
    );
    let usmap = Usmap::parse(&bytes).expect("parses");
    assert_eq!(usmap.extension_bytes, 8);
}

#[test]
fn oodle_and_brotli_are_refused_by_name() {
    for (method, name) in [(1u8, "Oodle"), (2, "Brotli")] {
        let bytes = file(header_prefix(4, &version_block(false), method), 0, 0, &[]);
        let error = Usmap::parse(&bytes).expect_err("unsupported");
        assert_eq!(
            error,
            UsmapError::UnsupportedCompression {
                method,
                name,
                offset: 7
            }
        );
        assert!(error.to_string().contains(name), "{error}");
    }
}
