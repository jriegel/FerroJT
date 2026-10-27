//! Command line surface: argument parsing, logging setup and the subcommands.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ferrojt::model::Format;
use ferrojt::{Error, Model};
use indicatif::{ProgressBar, ProgressStyle};

/// Read OpenJT files and convert them to glTF / USD.
#[derive(Debug, Parser)]
#[command(
    name = "ferrojt",
    version,
    about,
    long_about = None,
    propagate_version = true
)]
pub struct Cli {
    /// Repeat for more verbose output (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Also write logs to this file.
    #[arg(long, global = true, value_name = "FILE")]
    pub log: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the format and a summary of a file.
    Info(InfoArgs),
    /// Dump the parsed scene as JSON.
    Inspect(InspectArgs),
    /// Dump a file the way the classic JT tooling reports it.
    Dump(DumpArgs),
    /// Convert a file to another format.
    Convert(ConvertArgs),
    /// List the scene graph.
    Ls(LsArgs),
}

#[derive(Debug, clap::Args)]
pub struct InfoArgs {
    /// File to describe.
    pub input: PathBuf,

    /// Emit machine readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct InspectArgs {
    /// File to inspect.
    pub input: PathBuf,

    /// Pretty print the JSON.
    #[arg(short, long)]
    pub pretty: bool,
}

#[derive(Debug, clap::Args)]
pub struct DumpArgs {
    /// Files to dump. Several files are written as a JSON array.
    pub files: Vec<PathBuf>,

    /// Write the JSON to this file instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Leave the table of contents out of the dump.
    #[arg(long)]
    pub no_toc: bool,

    /// Leave the index arrays of every representation out of the dump.
    #[arg(long)]
    pub no_indices: bool,
}

#[derive(Debug, clap::Args)]
pub struct ConvertArgs {
    /// File to read.
    pub input: PathBuf,

    /// Output file. Defaults to the input name with the target extension.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Target format. Guessed from the output extension when omitted.
    #[arg(short, long, value_enum)]
    pub format: Option<TargetFormat>,

    /// Named layer filter to apply. Defaults to the filter the file declares
    /// as active, or to its first filter when the file leaves that unset.
    #[arg(long, value_name = "NAME", conflicts_with = "no_layer_filter")]
    pub layer_filter: Option<String>,

    /// Export every part and ignore the layer filters of the file.
    #[arg(long)]
    pub no_layer_filter: bool,

    /// Replace the output file if it exists.
    #[arg(short = 'y', long)]
    pub force: bool,
}

#[derive(Debug, clap::Args)]
pub struct LsArgs {
    /// File to walk.
    pub input: PathBuf,

    /// Do not draw a progress bar.
    #[arg(long)]
    pub no_progress: bool,
}

/// Formats `ferrojt` can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TargetFormat {
    /// glTF 2.0 binary container.
    Glb,
    /// glTF 2.0 JSON with a data URI buffer.
    Gltf,
    /// Universal Scene Description (not implemented yet).
    Usd,
}

impl From<TargetFormat> for Format {
    fn from(format: TargetFormat) -> Self {
        match format {
            TargetFormat::Glb => Format::Glb,
            TargetFormat::Gltf => Format::Gltf,
            TargetFormat::Usd => Format::Usd,
        }
    }
}

/// Install a `tracing` subscriber driven by `-v` / `--log`.
pub fn init_tracing(verbose: u8, log: Option<&Path>) -> Result<()> {
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = std::env::var("FERROJT_LOG").unwrap_or_else(|_| format!("ferrojt={level},warn"));
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));

    let registry = tracing_subscriber::fmt().with_env_filter(filter);
    match log {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating log file {}", path.display()))?;
            registry.with_writer(file).with_ansi(false).init();
        }
        None => registry.with_writer(std::io::stderr).init(),
    }
    Ok(())
}

pub fn info(args: &InfoArgs) -> Result<()> {
    let format = Format::detect(&args.input);
    let summary = serde_json::json!({
        "path": args.input.display().to_string(),
        "format": format.to_string(),
        "readable": format.is_readable(),
        "writable": format.is_writable(),
    });
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("{}  [{}]", args.input.display(), format);
        println!("  readable: {}", format.is_readable());
        println!("  writable: {}", format.is_writable());
    }
    // Reading still needs a JT implementation; report it as a clean message.
    match ferrojt::open(&args.input) {
        Ok(model) => print_summary(&args.input, &model, args.json)?,
        Err(err) => report_not_implemented(&err)?,
    }
    Ok(())
}

pub fn inspect(args: &InspectArgs) -> Result<()> {
    let model = load(&args.input)?;
    let value = serde_json::to_value(&model)?;
    let text = if args.pretty {
        serde_json::to_string_pretty(&value)?
    } else {
        serde_json::to_string(&value)?
    };
    println!("{text}");
    Ok(())
}

/// Dump files the way the classic JT tooling reports them.
///
/// Several files are written as a JSON array with the members of each object
/// indented one level deeper, which is what the reference tool does.
pub fn dump(args: &DumpArgs) -> Result<()> {
    if args.files.is_empty() {
        bail!("no input JT files given");
    }
    let mut config = ferrojt::jt::DUMP_ALL;
    if args.no_toc {
        config &= !ferrojt::jt::DUMP_TOC;
    }
    if args.no_indices {
        config &= !ferrojt::jt::DUMP_MESH;
    }

    let multiple = args.files.len() > 1;
    let indent = i32::from(multiple) + 1;

    let mut text = String::new();
    if multiple {
        text.push_str("[\n");
    }
    for (position, path) in args.files.iter().enumerate() {
        if position != 0 {
            text.push(',');
        }
        let file = ferrojt::jt::JtFile::open(path)
            .map_err(|err| map_error(path, err))
            .with_context(|| format!("reading {}", path.display()))?;
        text.push_str(&ferrojt::jt::dump(&file, config, indent)?);
    }
    if multiple {
        text.push_str("]\n");
    }

    match &args.output {
        Some(path) => {
            std::fs::write(path, &text).with_context(|| format!("writing {}", path.display()))?;
        }
        None => print!("{text}"),
    }
    Ok(())
}

pub fn convert(args: &ConvertArgs) -> Result<()> {
    let file = jt_file(&args.input)?;
    let model = if args.no_layer_filter {
        file.to_model_unfiltered()
    } else if let Some(name) = args.layer_filter.as_deref() {
        file.to_model_with_filter(name)?
    } else {
        file.to_model()
    };

    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(&args.input, args.format));
    let format = match args.format {
        Some(format) => format.into(),
        None => Format::detect(&output),
    };
    if !format.is_writable() {
        bail!("{format} cannot be written by ferrojt");
    }
    if output.exists() && !args.force {
        bail!("{} exists; pass --force to overwrite", output.display());
    }

    let bar = spinner(&format.to_string());
    ferrojt::export::write_to_file(format, &model, &output)
        .map_err(|err| anyhow::anyhow!("{err}"))
        .with_context(|| format!("writing {}", output.display()))?;
    finish(bar);

    println!(
        "{} -> {} [{}]",
        args.input.display(),
        output.display(),
        format
    );
    Ok(())
}

pub fn ls(args: &LsArgs) -> Result<()> {
    let model = load(&args.input)?;
    let bar = if args.no_progress {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(model.nodes.len() as u64).with_style(
            ProgressStyle::with_template("{wide_bar} {pos}/{len} nodes")
                .expect("valid progress template"),
        )
    };
    for scene in &model.scenes {
        println!("scene {}", scene.name.as_deref().unwrap_or("(default)"));
        for root in &scene.roots {
            print_node(&model, *root, 1, &bar);
        }
    }
    bar.finish_and_clear();
    println!(
        "\n{} nodes, {} meshes, {} materials, {} triangles",
        model.nodes.len(),
        model.meshes.len(),
        model.materials.len(),
        model.triangle_count()
    );
    Ok(())
}

fn print_node(model: &Model, index: usize, depth: usize, bar: &ProgressBar) {
    let Some(node) = model.nodes.get(index) else {
        eprintln!("{}node #{index} is missing", "  ".repeat(depth));
        return;
    };
    let indent = "  ".repeat(depth);
    let name = node.name.as_deref().unwrap_or("(unnamed)");
    let meshes = node.instances.len();
    println!("{indent}{name}  [{:?}]  meshes={meshes}", node.kind);
    bar.inc(1);
    for child in &node.children {
        print_node(model, *child, depth + 1, bar);
    }
}

fn load(path: &Path) -> Result<Model> {
    ferrojt::open(path)
        .map_err(|err| map_error(path, err))
        .with_context(|| format!("loading {}", path.display()))
}

/// Read a JT file without turning it into a model yet.
fn jt_file(path: &Path) -> Result<ferrojt::jt::JtFile> {
    ferrojt::jt::JtFile::open(path)
        .map_err(|err| map_error(path, err))
        .with_context(|| format!("loading {}", path.display()))
}

/// Turn the library's errors into friendly CLI messages.
fn map_error(path: &Path, err: Error) -> anyhow::Error {
    match err {
        Error::NotImplemented(what) => anyhow::anyhow!(
            "{what} is not implemented yet, so {} cannot be read. \
             The reader lands in `crates/ferrojt/src/jt`.",
            path.display()
        ),
        other => anyhow::Error::new(other).context(format!("reading {}", path.display())),
    }
}

/// Print the not-implemented note without failing the command.
fn report_not_implemented(err: &Error) -> Result<()> {
    match err {
        Error::NotImplemented(what) => {
            println!("  note: {what} is not implemented yet");
            Ok(())
        }
        other => Err(anyhow::anyhow!("{other}")),
    }
}

fn print_summary(path: &Path, model: &Model, json: bool) -> Result<()> {
    let summary = serde_json::json!({
        "nodes": model.nodes.len(),
        "meshes": model.meshes.len(),
        "materials": model.materials.len(),
        "textures": model.textures.len(),
        "vertices": model.vertex_count(),
        "triangles": model.triangle_count(),
        "unit": model.unit,
        "up_axis": model.up_axis,
        "bounds": model.bounds,
    });
    if json {
        let mut value = serde_json::json!({ "path": path.display().to_string() });
        if let (Some(object), Some(extra)) = (value.as_object_mut(), summary.as_object()) {
            object.extend(extra.clone());
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("  nodes:     {}", model.nodes.len());
        println!("  meshes:    {}", model.meshes.len());
        println!("  materials: {}", model.materials.len());
        println!("  triangles: {}", model.triangle_count());
    }
    Ok(())
}

fn default_output(input: &Path, format: Option<TargetFormat>) -> PathBuf {
    let format = format.map(Format::from).unwrap_or(Format::Glb);
    input.with_extension(format.extension())
}

fn spinner(label: &str) -> ProgressBar {
    let bar = ProgressBar::new_spinner().with_message(label.to_string());
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}

fn finish(bar: ProgressBar) {
    bar.finish_and_clear();
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn target_format_maps_to_model_format() {
        assert_eq!(Format::from(TargetFormat::Glb), Format::Glb);
        assert_eq!(Format::from(TargetFormat::Gltf), Format::Gltf);
        assert_eq!(Format::from(TargetFormat::Usd), Format::Usd);
    }

    #[test]
    fn default_output_uses_target_extension() {
        let path = Path::new("gear.jt");
        assert_eq!(
            default_output(path, Some(TargetFormat::Glb)),
            PathBuf::from("gear.glb")
        );
        assert_eq!(
            default_output(path, Some(TargetFormat::Usd)),
            PathBuf::from("gear.usda")
        );
    }

    fn parse_dump(args: &[&str]) -> DumpArgs {
        match Cli::try_parse_from(args).expect("valid arguments").command {
            Command::Dump(args) => args,
            other => panic!("expected the dump subcommand, got {other:?}"),
        }
    }

    #[test]
    fn dump_arguments_mirror_the_reference_tool() {
        let args = parse_dump(&["ferrojt", "dump", "one.jt"]);
        assert_eq!(args.files, [PathBuf::from("one.jt")]);
        assert_eq!(args.output, None);

        let args = parse_dump(&["ferrojt", "dump", "one.jt", "two.jt", "-o", "out.json"]);
        assert_eq!(args.files.len(), 2);
        assert_eq!(args.output, Some(PathBuf::from("out.json")));

        let args = parse_dump(&["ferrojt", "dump", "one.jt", "--no-toc", "--no-indices"]);
        assert!(args.no_toc);
        assert!(args.no_indices);
    }

    fn parse_convert(args: &[&str]) -> ConvertArgs {
        match Cli::try_parse_from(args).expect("valid arguments").command {
            Command::Convert(args) => args,
            other => panic!("expected the convert subcommand, got {other:?}"),
        }
    }

    #[test]
    fn convert_arguments_carry_the_layer_filter() {
        let args = parse_convert(&["ferrojt", "convert", "gear.jt"]);
        assert_eq!(args.layer_filter, None);
        assert!(!args.no_layer_filter);

        let args = parse_convert(&["ferrojt", "convert", "gear.jt", "--layer-filter", "LAYCUR"]);
        assert_eq!(args.layer_filter.as_deref(), Some("LAYCUR"));
        assert!(!args.no_layer_filter);

        let args = parse_convert(&["ferrojt", "convert", "gear.jt", "--no-layer-filter"]);
        assert_eq!(args.layer_filter, None);
        assert!(args.no_layer_filter);
    }

    #[test]
    fn a_named_filter_and_no_filter_cannot_be_combined() {
        let error = Cli::try_parse_from([
            "ferrojt",
            "convert",
            "gear.jt",
            "--layer-filter",
            "ALL",
            "--no-layer-filter",
        ])
        .expect_err("the two filters conflict");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "{error}"
        );
    }

    /// Several files become one JSON array, which is what the reference tool
    /// writes and what the tests compare against.
    ///
    /// Only samples that have a reference dump are used. `SampleJT_Server.jt`
    /// has a `Shape_Vertex` child under an `Instance`, and the reference writer
    /// has no branch for that node type: it emits the separator comma and then
    /// nothing at all, which leaves a dangling comma in its own output. The
    /// Rust writer reproduces that byte for byte, so the sample cannot be
    /// parsed as JSON and is left to the byte-for-byte comparison in
    /// `crates/ferrojt/tests/dump.rs`.
    #[test]
    fn dumping_several_files_produces_a_json_array() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root")
            .join("data")
            .join("ExampleFiles");
        let reference = root.parent().expect("data directory").join("Reference");
        let has_reference = |path: &Path| {
            let stem = path.file_stem().expect("stem").to_string_lossy();
            reference.join(format!("{stem}.json")).is_file()
        };
        let mut files = std::fs::read_dir(&root)
            .expect("sample directory")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jt") && has_reference(path))
            .collect::<Vec<_>>();
        files.sort();
        files.truncate(2);
        if files.len() < 2 {
            return;
        }

        let directory = std::env::temp_dir().join("ferrojt-cli-dump-test");
        std::fs::create_dir_all(&directory).expect("temporary directory");
        let output = directory.join("array.json");
        let args = DumpArgs {
            files,
            output: Some(output.clone()),
            no_toc: false,
            no_indices: false,
        };
        dump(&args).expect("dumping the samples");

        let text = std::fs::read_to_string(&output).expect("dump output");
        let value: serde_json::Value = serde_json::from_str(&text).expect("the dump is JSON");
        assert_eq!(value.as_array().map(Vec::len), Some(2));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
