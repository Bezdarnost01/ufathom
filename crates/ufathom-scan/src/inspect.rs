//! Static inspection of an executable file: no process involved.

use std::path::Path;

use ufathom_mem::ModuleInfo;

use crate::error::ScanError;
use crate::pe::{PeImage, load_pe_file};
use crate::version::{VersionFacts, detect_version};

/// Section names written by well-known packers and protectors, lowercase.
///
/// Each is a name the tool is documented to create. A legitimate build can
/// reuse one by accident, so a match is an indicator, never a verdict.
const PACKER_SECTION_NAMES: &[(&str, &str)] = &[
    ("upx0", "UPX"),
    ("upx1", "UPX"),
    ("upx2", "UPX"),
    (".vmp0", "VMProtect"),
    (".vmp1", "VMProtect"),
    (".vmp2", "VMProtect"),
    (".themida", "Themida"),
    (".enigma1", "Enigma Protector"),
    (".enigma2", "Enigma Protector"),
    (".aspack", "ASPack"),
    (".adata", "ASPack"),
    (".mpress1", "MPRESS"),
    (".mpress2", "MPRESS"),
    (".petite", "Petite"),
    (
        ".bind",
        "a DRM wrapper that keeps code encrypted until it runs",
    ),
];

/// Whether an image looks packed, and why.
///
/// Built from section facts only, with no entropy measurement. An image that
/// does not look packed may still be; one that does may be a false alarm.
/// What it means for a scan: strings and resources the static pass reads may
/// be missing or wrong until the image is unpacked at run time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PackingHints {
    /// Human-readable reasons, in a fixed order. Empty when nothing stands out.
    pub indicators: Vec<String>,
}

impl PackingHints {
    /// Whether any indicator was found.
    #[must_use]
    pub fn looks_packed(&self) -> bool {
        !self.indicators.is_empty()
    }

    /// Looks at the section table and the entry point of `image`.
    #[must_use]
    pub fn of(image: &PeImage) -> Self {
        let mut indicators = Vec::new();
        for section in &image.sections {
            let lowered = section.name.to_ascii_lowercase();
            if let Some((_, tool)) = PACKER_SECTION_NAMES
                .iter()
                .find(|(name, _)| *name == lowered.as_str())
            {
                indicators.push(format!("section `{}` is named by {tool}", section.name));
            }
            if section.is_executable() && section.is_writable() {
                indicators.push(format!(
                    "section `{}` is both writable and executable",
                    section.name
                ));
            }
            if section.is_executable() && section.raw_size == 0 && section.virtual_size != 0 {
                indicators.push(format!(
                    "executable section `{}` has no data in the file; its code appears at run time",
                    section.name
                ));
            }
        }
        if image.entry_point_rva != 0 {
            match image.section_at_rva(image.entry_point_rva) {
                None => indicators.push(format!(
                    "the entry point {:#x} is outside every section",
                    image.entry_point_rva
                )),
                Some(section) if !section.is_executable() => indicators.push(format!(
                    "the entry point is in non-executable section `{}`",
                    section.name
                )),
                Some(_) => {}
            }
        }
        Self { indicators }
    }
}

/// What a static look at an executable found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeFacts {
    /// The module as the file maps it.
    pub module: ModuleInfo,
    /// Headers and sections. [`PeImage::base`] is the mapped address.
    pub image: PeImage,
    /// Engine version evidence.
    pub version: VersionFacts,
    /// Whether the image looks packed.
    pub packing: PackingHints,
}

/// Reads the PE file at `path` and reports what can be learned without
/// running it: sections, engine version evidence, and whether it looks
/// packed.
///
/// The file is mapped into a snapshot with [`load_pe_file`], which reads each
/// section once, so this costs about the size of the file in memory. The file
/// is never modified or executed.
///
/// # Errors
///
/// Any error of [`load_pe_file`] or [`PeImage::parse_from_memory`]: the file
/// cannot be read, is not a PE image, is truncated, or is malformed. Missing
/// version evidence is not an error.
pub fn inspect_exe(path: impl AsRef<Path>) -> Result<ExeFacts, ScanError> {
    let (snapshot, module) = load_pe_file(path)?;
    let image = PeImage::parse_from_memory(&snapshot, module.base)?;
    let version = detect_version(&snapshot, &image);
    let packing = PackingHints::of(&image);
    Ok(ExeFacts {
        module,
        image,
        version,
        packing,
    })
}
