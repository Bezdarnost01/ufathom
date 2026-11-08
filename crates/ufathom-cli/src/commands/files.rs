//! Commands that work on dump files: `gen`, `usmap`, `check-oracle`, `diff`.

use anyhow::{Context, anyhow};
use ufathom_model::diff;
use ufathom_usmap::{Usmap, compare, write_usmap_with_report};

use crate::args::{CheckOracleArgs, DiffArgs, GenArgs, UsmapArgs};
use crate::commands::io::{
    generate_sdk, print_gen_report, read_dump, runtime_options, size, write_file,
};
use crate::error::{CliError, Outcome};

/// `ufathom gen`.
pub fn generate(args: &GenArgs) -> Outcome {
    let options = runtime_options(&args.runtime)?;
    let dump = read_dump(&args.dump)?;
    let report = generate_sdk(&dump, &options, &args.output)?;
    print_gen_report(&report, &args.output);
    Ok(())
}

/// `ufathom usmap`.
pub fn usmap(args: &UsmapArgs) -> Outcome {
    let dump = read_dump(&args.dump)?;
    let (bytes, report) = write_usmap_with_report(&dump);
    write_file(&args.output, &bytes)?;
    println!(
        "wrote:         {} ({})",
        args.output.display(),
        size(bytes.len())
    );
    println!(
        "left out:      {} duplicate structs, {} duplicate enums, {} functions (usmap holds data members only)",
        report.duplicate_structs_skipped.len(),
        report.duplicate_enums_skipped.len(),
        report.functions_omitted
    );
    let changed = report.unknown_property_types.len()
        + report.array_dims_clamped.len()
        + report.limits_exceeded.len();
    if changed > 0 {
        println!(
            "changed:       {} unknown property types, {} arrays clamped to 255, {} items over a format limit",
            report.unknown_property_types.len(),
            report.array_dims_clamped.len(),
            report.limits_exceeded.len()
        );
    }
    Ok(())
}

/// `ufathom check-oracle`.
///
/// The exit code is 0 when the dump satisfies the usmap and 1 when it does
/// not, so the command can gate a script.
pub fn check_oracle(args: &CheckOracleArgs) -> Outcome {
    let dump = read_dump(&args.dump)?;
    let bytes = std::fs::read(&args.usmap)
        .with_context(|| format!("cannot read the usmap file {}", args.usmap.display()))?;
    let usmap = Usmap::parse(&bytes)
        .map_err(|error| anyhow!("{} is not a usable usmap: {error}", args.usmap.display()))?;
    let report = compare(&dump, &usmap);
    if args.json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|error| anyhow!("cannot write the report as JSON: {error}"))?;
        println!("{text}");
    } else {
        print!("{report}");
    }
    if report.is_clean() {
        Ok(())
    } else {
        Err(CliError::Failure(anyhow!(
            "the dump does not satisfy the usmap (see the report above)"
        )))
    }
}

/// `ufathom diff`. Differences are the point of the command, so they do not
/// change the exit code.
pub fn diff_dumps(args: &DiffArgs) -> Outcome {
    let old = read_dump(&args.old)?;
    let new = read_dump(&args.new)?;
    let result = diff(&old, &new);
    if args.json {
        let text = serde_json::to_string_pretty(&result)
            .map_err(|error| anyhow!("cannot write the diff as JSON: {error}"))?;
        println!("{text}");
    } else {
        print!("{result}");
    }
    Ok(())
}
