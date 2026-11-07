//! Engine version detection from a PE image.
//!
//! Three kinds of evidence, none of which is trusted alone:
//!
//! 1. **The version resource**, found through the resource directory's
//!    `RT_VERSION` branch: `ProductVersion`, `FileVersion` and the fixed
//!    binary numbers. This is a *claim*: a project may write its own number
//!    there.
//! 2. **Build strings** of the form `++Project+Branch-CL-N`, found in
//!    read-only sections. The engine's own branch string
//!    (`++UE5+Release-5.4`) is a second claim.
//! 3. **Marker strings** ([`MARKERS`]) that exist only in engines of a given
//!    version or later. These give a *lower bound* that does not depend on
//!    what the binary says about itself.
//!
//! [`detect_version`] never fails. Whatever cannot be read becomes a line of
//! evidence saying so. A claim and a bound that disagree are both kept, and
//! the disagreement is stated.

mod markers;
mod resource;
mod scan;
mod text;
mod versioninfo;

use std::cmp::Reverse;

use ufathom_mem::MemoryReader;
use ufathom_model::EngineInfo;

pub use markers::{MARKERS, Marker};
pub use scan::MarkerHit;
pub use text::{BuildString, PartialVersion, StringEncoding, engine_version_from_text};
pub use versioninfo::{FixedFileInfo, VersionResource, VersionString};

use crate::pe::PeImage;

/// What was learned about the engine version of one image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionFacts {
    /// The version resource, when the image has a readable one.
    pub resource: Option<VersionResource>,
    /// The version the binary claims, parsed. [`EngineInfo::claimed`] is its
    /// text.
    pub claimed: Option<PartialVersion>,
    /// Build strings, best first. The first is [`EngineInfo::build`].
    pub build_strings: Vec<BuildString>,
    /// Marker strings found.
    pub markers: Vec<MarkerHit>,
    /// Problems met while looking: a damaged resource tree, unreadable
    /// sections, limits reached. Also present in [`EngineInfo::evidence`].
    pub notes: Vec<String>,
    /// The result in the form the dump records.
    pub engine: EngineInfo,
}

/// Looks for engine version evidence in `image`, reading through `mem`.
///
/// Only reads. Never fails: an image with no resource, no build string and no
/// marker gives an [`EngineInfo`] with every field absent and evidence that
/// says what was looked at. [`EngineInfo::max_version`] is never set: no
/// marker proves an upper bound.
///
/// # Claimed version
///
/// [`EngineInfo::claimed`] is the numeric engine version taken from the
/// first of these that states one, in order: the `ProductVersion` string, the
/// `FileVersion` string, other strings whose key ends in `Version`, the fixed
/// product version, the fixed file version, then an engine branch build
/// string. It is normalised (`4.13.2`, `5.4`); the text it came from is in
/// the evidence. If sources state different versions, the first wins and the
/// others are listed as disagreeing.
///
/// # Build
///
/// [`EngineInfo::build`] is the best build string: one with a changelist
/// before one without, UTF-16 before ASCII, then the lexicographically
/// smallest text. Others are listed in the evidence.
#[must_use]
pub fn detect_version(mem: &dyn MemoryReader, image: &PeImage) -> VersionFacts {
    let mut notes = Vec::new();
    let resource = match resource::read_version_resource(mem, image) {
        Ok(resource) => resource,
        Err(error) => {
            tracing::debug!(%error, "version resource unreadable");
            notes.push(format!("version resource not read: {error}"));
            None
        }
    };
    let scan::ScanOutput {
        mut builds,
        hits,
        notes: scan_notes,
    } = scan::scan_sections(mem, image);
    notes.extend(scan_notes);
    builds.sort_by(|a, b| {
        (
            Reverse(a.changelist.is_some()),
            Reverse(a.encoding),
            &a.text,
        )
            .cmp(&(
                Reverse(b.changelist.is_some()),
                Reverse(b.encoding),
                &b.text,
            ))
    });

    let mut evidence = Vec::new();
    let sources = claim_sources(resource.as_ref(), &builds, &mut evidence);
    let claimed = sources.first().map(|source| source.version);
    describe_claims(&sources, &mut evidence);
    describe_builds(&builds, &mut evidence);

    let min_version = hits.iter().map(|hit| hit.minimum).max();
    for hit in &hits {
        evidence.push(format!(
            "string `{}` ({}) in {} at RVA {:#x}: {}, so the engine is at least {}.{}",
            hit.name,
            hit.encoding,
            hit.section,
            hit.rva,
            hit.reason,
            hit.minimum.major,
            hit.minimum.minor
        ));
    }
    if let (Some(claim), Some(bound)) = (claimed, min_version)
        && claim.branch() < (bound.major, bound.minor)
    {
        evidence.push(format!(
            "contradiction: the claimed version {claim} is below the lower bound {}.{} \
             that the marker strings imply; both are kept",
            bound.major, bound.minor
        ));
    }
    if let Some(problems) = resource.as_ref().map(|r| &r.problems) {
        for problem in problems {
            evidence.push(format!("version resource damaged: {problem}"));
        }
    }
    for note in &notes {
        evidence.push(format!("note: {note}"));
    }

    let engine = EngineInfo {
        claimed: claimed.map(|version| version.to_string()),
        build: builds.first().map(|build| build.text.clone()),
        min_version,
        max_version: None,
        evidence,
    };
    VersionFacts {
        resource,
        claimed,
        build_strings: builds,
        markers: hits,
        notes,
        engine,
    }
}

/// Somewhere a version claim came from.
struct ClaimSource {
    label: String,
    version: PartialVersion,
}

/// Lists every place that states an engine version, best source first, and
/// adds a line to `evidence` for each resource string that does not.
fn claim_sources(
    resource: Option<&VersionResource>,
    builds: &[BuildString],
    evidence: &mut Vec<String>,
) -> Vec<ClaimSource> {
    let mut sources = Vec::new();
    match resource {
        None => evidence.push("no version resource".to_owned()),
        Some(resource) => {
            let mut keys: Vec<&str> = vec!["ProductVersion", "FileVersion"];
            for entry in &resource.strings {
                if entry.key.ends_with("Version") && !keys.contains(&entry.key.as_str()) {
                    keys.push(entry.key.as_str());
                }
            }
            for key in keys {
                let Some(value) = resource.string(key) else {
                    continue;
                };
                match engine_version_from_text(value) {
                    Some(version) => sources.push(ClaimSource {
                        label: format!("version resource {key} \"{value}\""),
                        version,
                    }),
                    None => evidence.push(format!(
                        "version resource {key} \"{value}\" does not state an engine version"
                    )),
                }
            }
            if let Some(fixed) = &resource.fixed {
                for (label, text) in [
                    ("product", fixed.product_version_text()),
                    ("file", fixed.file_version_text()),
                ] {
                    match engine_version_from_text(&text) {
                        Some(version) => sources.push(ClaimSource {
                            label: format!("fixed {label} version {text}"),
                            version,
                        }),
                        None => evidence.push(format!(
                            "fixed {label} version {text} does not state an engine version"
                        )),
                    }
                }
            }
        }
    }
    for build in builds {
        if let Some(version) = build.engine_release {
            sources.push(ClaimSource {
                label: format!("engine branch string `{}` ({})", build.text, build.encoding),
                version,
            });
        }
    }
    sources
}

/// Adds the lines that say which source was used and where others disagree.
fn describe_claims(sources: &[ClaimSource], evidence: &mut Vec<String>) {
    let Some((first, rest)) = sources.split_first() else {
        evidence.push("no source states an engine version".to_owned());
        return;
    };
    evidence.push(format!(
        "claimed version {} from {}",
        first.version, first.label
    ));
    for other in rest {
        if other.version.branch() == first.version.branch() {
            evidence.push(format!("{} agrees: {}", other.label, other.version));
        } else {
            evidence.push(format!(
                "disagreement: {} states {}, but the claim above is {}",
                other.label, other.version, first.version
            ));
        }
    }
}

/// Adds one line per build string, saying which one was chosen.
fn describe_builds(builds: &[BuildString], evidence: &mut Vec<String>) {
    for (index, build) in builds.iter().enumerate() {
        let role = if index == 0 {
            "build string"
        } else {
            "other build string"
        };
        let changelist = build.changelist.map_or_else(
            || "no changelist".to_owned(),
            |cl| format!("changelist {cl}"),
        );
        evidence.push(format!(
            "{role} `{}` ({}) in {} at RVA {:#x}, {changelist}",
            build.text, build.encoding, build.section, build.rva
        ));
    }
    if builds.is_empty() {
        evidence.push("no build string found".to_owned());
    }
}
