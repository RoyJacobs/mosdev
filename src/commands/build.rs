use std::io::{Read, Write};
use std::path::PathBuf;
use std::str::FromStr;

use clap::{App, Arg, ArgMatches};
use fs_err as fs;
use itertools::Itertools;

use crate::core::codegen::{codegen, CodegenOptions};
use crate::core::io::{to_vice_symbols, SegmentMerger};
use crate::core::parser;
use crate::errors::{MosError, MosResult};

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SymbolType {
    Vice,
}

impl FromStr for SymbolType {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "vice" => Ok(SymbolType::Vice),
            _ => Err("no match"),
        }
    }
}

pub fn build_app() -> App<'static> {
    App::new("build")
        .about("Assembles input file(s)")
        .arg(
            Arg::new("input")
                .about("Sets the input file to use")
                .required(true)
                .multiple(true),
        )
        .arg(
            Arg::new("target-dir")
                .about("Directory for generated files")
                .long("target-dir")
                .default_value("."),
        )
        .arg(
            Arg::new("symbols")
                .about("Generate symbols")
                .case_insensitive(true)
                .long("symbols")
                .possible_values(&["vice"]),
        )
}

pub fn build_command(args: &ArgMatches) -> MosResult<()> {
    let input_names = args.values_of("input").unwrap().collect_vec();
    let target_dir = PathBuf::from(args.value_of("target-dir").unwrap());

    for input_name in input_names {
        let input_path = PathBuf::from(input_name);
        let output_path = PathBuf::from(format!(
            "{}.prg",
            input_path.file_stem().unwrap().to_string_lossy()
        ));
        let symbol_path = PathBuf::from(format!(
            "{}.vs",
            input_path.file_stem().unwrap().to_string_lossy()
        ));

        let mut file = fs::File::open(&input_path)?;
        let mut source = String::new();
        file.read_to_string(&mut source)?;

        let (tree, error) = parser::parse(&input_path, source.as_str());
        if let Some(e) = error {
            return Err(e);
        }
        let generated_code = codegen(tree, CodegenOptions { pc: 0x2000.into() })?;

        let mut merger = SegmentMerger::new(output_path);
        for segment_name in generated_code.segments().keys() {
            let segment = generated_code.segments().get(segment_name);
            if segment.options().write {
                merger.merge(segment_name, segment)?;
            }
        }

        if merger.has_errors() {
            return Err(MosError::Multiple(merger.errors()));
        }

        for (path, m) in merger.targets() {
            if let Some(range) = &m.range() {
                log::trace!("Writing: (${:04x} - ${:04x})", range.start, range.end);
                log::trace!("Writing: {:?}", m.range_data());
                let mut out = fs::File::create(target_dir.join(path))?;
                out.write_all(&(range.start as u16).to_le_bytes())?;
                out.write_all(&m.range_data())?;
            }
        }

        if args
            .values_of_t::<SymbolType>("symbols")
            .unwrap_or_else(|_| vec![])
            .contains(&SymbolType::Vice)
        {
            let mut out = fs::File::create(target_dir.join(symbol_path))?;
            out.write_all(to_vice_symbols(generated_code.symbol_table()).as_bytes())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;
    use itertools::Itertools;

    use crate::commands::{build_app, build_command};

    #[test]
    fn can_invoke_build() -> Result<()> {
        let root = env!("CARGO_MANIFEST_DIR");
        let input = &format!("{}/test/cli/build/valid.asm", root);

        let args = build_app().get_matches_from(vec![
            "build",
            input,
            "--target-dir",
            &format!("{}/target", root),
            "--symbols",
            "vice",
        ]);
        build_command(&args)?;

        let out_path = &format!("{}/target/valid.prg", root);
        let out_bytes = std::fs::read(out_path)?;
        let prg_path = &format!("{}/test/cli/build/valid.prg", root);
        let prg_bytes = std::fs::read(prg_path)?;
        assert_eq!(out_bytes, prg_bytes);

        let vs_path = &format!("{}/target/valid.vs", root);
        let vs_bytes = std::fs::read_to_string(vs_path)?;
        let vs_lines = vs_bytes.lines().collect_vec();
        assert_eq!(vs_lines, vec!["al C:2007 .data"]);

        Ok(())
    }

    #[test]
    fn build_multiple_segments() -> Result<()> {
        build_and_compare("multiple_segments.asm")
    }

    #[test]
    fn build_include() -> Result<()> {
        build_and_compare("include.asm")
    }

    fn build_and_compare(input: &str) -> Result<()> {
        let root = env!("CARGO_MANIFEST_DIR");
        let full_input_path = &format!("{}/test/cli/build/{}", root, input);

        let args = build_app().get_matches_from(vec![
            "build",
            full_input_path.as_str(),
            "--target-dir",
            &format!("{}/target", root),
        ]);
        build_command(&args)?;

        let actual_path = &format!(
            "{}/target/{}",
            root,
            PathBuf::from(input).with_extension("prg").to_string_lossy()
        );
        let actual_bytes = std::fs::read(actual_path)?;
        let expected_prg_path = PathBuf::from(full_input_path)
            .with_extension("prg")
            .into_os_string();
        let expected_prg_bytes = std::fs::read(expected_prg_path)?;
        assert_eq!(actual_bytes, expected_prg_bytes);

        Ok(())
    }
}
