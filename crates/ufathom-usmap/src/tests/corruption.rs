//! Hostile and damaged input must produce errors, never panics.

use ufathom_model::PropertyType;

use super::fixtures::{
    Bytes, class_with, dump_of, enumeration, every_property_type, file, header_prefix,
};
use crate::{MAX_DECOMPRESSED_SIZE, MAX_TYPE_DEPTH, Usmap, UsmapError, write_usmap};

/// A valid version 4 file with a few types.
fn small_valid_file() -> Vec<u8> {
    let dump = dump_of(
        vec![class_with(
            "Thing",
            Some("Object"),
            &[
                ("Count", PropertyType::Int32),
                (
                    "Items",
                    PropertyType::Array {
                        inner: Box::new(PropertyType::Name),
                    },
                ),
            ],
        )],
        vec![enumeration("EMode", &[("A", 0), ("B", 1)])],
    );
    write_usmap(&dump)
}

fn zstd(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

/// The payload of `small_valid_file`, without its header.
fn small_payload() -> Vec<u8> {
    small_valid_file()[16..].to_vec()
}

fn zstd_file(declared_decompressed: u32, payload: &[u8]) -> Vec<u8> {
    let compressed = zstd(payload);
    file(
        header_prefix(4, &0u32.to_le_bytes(), 3),
        u32::try_from(compressed.len()).expect("size"),
        declared_decompressed,
        &compressed,
    )
}

fn uncompressed_file(body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len()).expect("size");
    file(header_prefix(4, &0u32.to_le_bytes(), 0), size, size, body)
}

#[test]
fn the_valid_file_really_is_valid() {
    Usmap::parse(&small_valid_file()).expect("parses");
    let payload = small_payload();
    Usmap::parse(&zstd_file(
        u32::try_from(payload.len()).expect("size"),
        &payload,
    ))
    .expect("parses");
}

#[test]
fn every_truncation_of_an_uncompressed_file_is_an_error() {
    let bytes = small_valid_file();
    for length in 0..bytes.len() {
        let result = Usmap::parse(&bytes[..length]);
        assert!(result.is_err(), "prefix of {length} bytes parsed");
    }
}

#[test]
fn every_truncation_of_a_zstd_file_is_an_error() {
    let payload = small_payload();
    let bytes = zstd_file(u32::try_from(payload.len()).expect("size"), &payload);
    for length in 0..bytes.len() {
        let result = Usmap::parse(&bytes[..length]);
        assert!(result.is_err(), "prefix of {length} bytes parsed");
    }
}

#[test]
fn every_truncation_of_a_body_with_an_honest_header_is_an_error() {
    // The header is rewritten to match the shortened body, so the error has
    // to come from the body parser and not from the size check.
    let payload = small_payload();
    for length in 0..payload.len() {
        let bytes = uncompressed_file(&payload[..length]);
        assert!(
            Usmap::parse(&bytes).is_err(),
            "body of {length} bytes parsed"
        );
    }
}

#[test]
fn every_single_byte_corruption_is_survived() {
    // No assertion on the outcome: some corruptions are still valid files.
    // The test is that none panics or hangs.
    let bytes = small_valid_file();
    for position in 0..bytes.len() {
        for replacement in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
            let mut damaged = bytes.clone();
            damaged[position] = replacement;
            let _ = Usmap::parse(&damaged);
        }
    }
}

#[test]
fn bad_magic_is_rejected() {
    let mut bytes = small_valid_file();
    bytes[0] = 0x00;
    assert_eq!(
        Usmap::parse(&bytes),
        Err(UsmapError::BadMagic { found: 0x3000 })
    );
}

#[test]
fn empty_input_is_truncated() {
    assert!(matches!(
        Usmap::parse(&[]),
        Err(UsmapError::Truncated { offset: 0, .. })
    ));
}

#[test]
fn unknown_versions_are_rejected() {
    for version in [5u8, 6, 100, 255] {
        let mut bytes = small_valid_file();
        bytes[2] = version;
        assert_eq!(
            Usmap::parse(&bytes),
            Err(UsmapError::UnsupportedVersion { version })
        );
    }
}

#[test]
fn unknown_compression_methods_are_rejected() {
    for method in [4u8, 9, 255] {
        let mut bytes = small_valid_file();
        bytes[7] = method;
        assert_eq!(
            Usmap::parse(&bytes),
            Err(UsmapError::UnsupportedCompression {
                method,
                name: "unknown",
                offset: 7
            })
        );
    }
}

#[test]
fn a_name_index_out_of_range_is_rejected_with_its_offset() {
    // One name, one enum whose name index is 1.
    let body = Bytes::default()
        .u32(1)
        .u16(1)
        .raw(b"A")
        .u32(1) // enums
        .u32(1) // name index 1, only 0 exists
        .u16(0)
        .u32(0) // structs
        .0;
    let error = Usmap::parse(&uncompressed_file(&body)).expect_err("bad index");
    assert_eq!(
        error,
        UsmapError::NameIndexOutOfRange {
            index: 1,
            offset: 16 + 4 + 2 + 1 + 4,
            count: 1
        }
    );
}

#[test]
fn the_index_that_means_no_parent_is_only_allowed_as_a_parent() {
    // 0xFFFFFFFF as a struct name is an out-of-range index, not "no name".
    let body = Bytes::default()
        .u32(1)
        .u16(1)
        .raw(b"A")
        .u32(0) // enums
        .u32(1)
        .u32(u32::MAX)
        .u32(u32::MAX)
        .u16(0)
        .u16(0)
        .0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::NameIndexOutOfRange {
            index: u32::MAX,
            ..
        })
    ));
}

#[test]
fn huge_counts_are_rejected_before_allocating() {
    // Names.
    let body = Bytes::default().u32(u32::MAX).0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::CountTooLarge { what: "name", .. })
    ));

    // Enums.
    let body = Bytes::default().u32(0).u32(0x4000_0000).0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::CountTooLarge { what: "enum", .. })
    ));

    // Enum values.
    let body = Bytes::default()
        .u32(1)
        .u16(1)
        .raw(b"E")
        .u32(1)
        .u32(0)
        .u16(u16::MAX)
        .0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::CountTooLarge {
            what: "enum value",
            ..
        })
    ));

    // Structs.
    let body = Bytes::default().u32(0).u32(0).u32(u32::MAX).0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::CountTooLarge { what: "struct", .. })
    ));

    // Properties.
    let body = Bytes::default()
        .u32(1)
        .u16(1)
        .raw(b"S")
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(u32::MAX)
        .u16(0)
        .u16(u16::MAX)
        .0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::CountTooLarge {
            what: "property",
            ..
        })
    ));

    // Custom versions in the header.
    let block = Bytes::default().u32(1).i32(0).i32(0).u32(u32::MAX).0;
    let bytes = file(header_prefix(4, &block, 0), 0, 0, &[]);
    assert!(matches!(
        Usmap::parse(&bytes),
        Err(UsmapError::CountTooLarge {
            what: "custom version",
            ..
        })
    ));
}

#[test]
fn a_decompressed_size_above_the_limit_is_rejected_before_decompressing() {
    for declared in [MAX_DECOMPRESSED_SIZE + 1, u32::MAX] {
        let bytes = zstd_file(declared, &small_payload());
        assert_eq!(
            Usmap::parse(&bytes),
            Err(UsmapError::DecompressedSizeTooLarge {
                declared,
                limit: MAX_DECOMPRESSED_SIZE
            })
        );
    }
}

#[test]
fn a_decompressed_size_lie_is_rejected() {
    let payload = small_payload();
    let real = u32::try_from(payload.len()).expect("size");

    // Claims more than the stream holds.
    let error = Usmap::parse(&zstd_file(real + 10, &payload)).expect_err("lie");
    assert_eq!(
        error,
        UsmapError::DecompressedSizeMismatch {
            declared: real + 10,
            actual: payload.len()
        }
    );

    // Claims less: decoding stops one byte past the claim.
    let error = Usmap::parse(&zstd_file(real - 10, &payload)).expect_err("lie");
    assert_eq!(
        error,
        UsmapError::DecompressedSizeMismatch {
            declared: real - 10,
            actual: payload.len() - 9
        }
    );

    // A stream that expands far beyond its claim is never fully expanded.
    let bomb = vec![0u8; 8 * 1024 * 1024];
    let error = Usmap::parse(&zstd_file(1024, &bomb)).expect_err("bomb");
    assert_eq!(
        error,
        UsmapError::DecompressedSizeMismatch {
            declared: 1024,
            actual: 1025
        }
    );
}

#[test]
fn uncompressed_size_fields_must_agree() {
    let body = small_payload();
    let size = u32::try_from(body.len()).expect("size");
    let bytes = file(
        header_prefix(4, &0u32.to_le_bytes(), 0),
        size,
        size + 1,
        &body,
    );
    assert_eq!(
        Usmap::parse(&bytes),
        Err(UsmapError::UncompressedSizeMismatch {
            compressed: size,
            decompressed: size + 1
        })
    );
}

#[test]
fn a_payload_larger_than_the_file_is_rejected() {
    let body = small_payload();
    let size = u32::try_from(body.len()).expect("size");
    let bytes = file(
        header_prefix(4, &0u32.to_le_bytes(), 0),
        size + 5,
        size + 5,
        &body,
    );
    assert_eq!(
        Usmap::parse(&bytes),
        Err(UsmapError::PayloadTruncated {
            offset: 16,
            declared: size + 5,
            available: body.len()
        })
    );
}

#[test]
fn garbage_that_claims_to_be_zstd_is_rejected() {
    let garbage = vec![0xAA; 64];
    let bytes = file(header_prefix(4, &0u32.to_le_bytes(), 3), 64, 100, &garbage);
    assert!(matches!(
        Usmap::parse(&bytes),
        Err(UsmapError::Decompress { .. })
    ));
}

#[test]
fn invalid_utf8_in_a_name_is_rejected_with_its_offset() {
    let body = Bytes::default()
        .u32(1)
        .u16(2)
        .raw(&[0xFF, 0xFE])
        .u32(0)
        .u32(0)
        .0;
    assert_eq!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::InvalidUtf8 { offset: 16 + 6 })
    );
}

#[test]
fn an_unknown_property_type_tag_is_rejected_with_its_offset() {
    let body = Bytes::default()
        .u32(2)
        .u16(1)
        .raw(b"S")
        .u16(1)
        .raw(b"P")
        .u32(0) // enums
        .u32(1)
        .u32(0) // struct S
        .u32(u32::MAX)
        .u16(1)
        .u16(1)
        .u16(0)
        .u8(1)
        .u32(1) // property P
        .u8(0x55) // no such type
        .0;
    let offset = 16 + body.len() - 1;
    assert_eq!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::UnknownPropertyType { tag: 0x55, offset })
    );
}

#[test]
fn deeply_nested_types_are_rejected_without_recursing_forever() {
    let mut body = Bytes::default()
        .u32(2)
        .u16(1)
        .raw(b"S")
        .u16(1)
        .raw(b"P")
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(u32::MAX)
        .u16(1)
        .u16(1)
        .u16(0)
        .u8(1)
        .u32(1);
    for _ in 0..(MAX_TYPE_DEPTH + 10) {
        body = body.u8(8); // Array of
    }
    let body = body.u8(2).0;
    assert!(matches!(
        Usmap::parse(&uncompressed_file(&body)),
        Err(UsmapError::TypeNestingTooDeep {
            limit: MAX_TYPE_DEPTH,
            ..
        })
    ));

    // Exactly at the limit is fine.
    let mut body = Bytes::default()
        .u32(2)
        .u16(1)
        .raw(b"S")
        .u16(1)
        .raw(b"P")
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(u32::MAX)
        .u16(1)
        .u16(1)
        .u16(0)
        .u8(1)
        .u32(1);
    for _ in 0..(MAX_TYPE_DEPTH - 1) {
        body = body.u8(8);
    }
    let body = body.u8(2).0;
    Usmap::parse(&uncompressed_file(&body)).expect("depth at the limit parses");
}

#[test]
fn a_file_with_every_property_type_survives_all_truncations() {
    let cases = every_property_type();
    let fields: Vec<(String, PropertyType)> = cases
        .iter()
        .enumerate()
        .map(|(position, (ty, _))| (format!("F{position}"), ty.clone()))
        .collect();
    let refs: Vec<(&str, PropertyType)> = fields
        .iter()
        .map(|(name, ty)| (name.as_str(), ty.clone()))
        .collect();
    let bytes = write_usmap(&dump_of(vec![class_with("All", None, &refs)], vec![]));
    for length in 0..bytes.len() {
        assert!(Usmap::parse(&bytes[..length]).is_err(), "prefix {length}");
    }
}
