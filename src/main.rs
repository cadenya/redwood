use std::path::PathBuf;

use anyhow::{Context, Result};

use clap::{Parser, ValueEnum};

use redwood::backends::{
    cli::CliBackend, docs::DocsBackend, golang::GoBackend, manifest::ManifestBackend,
    openapi_export::OpenApiBackend, python::PythonBackend, ruby::RubyBackend,
    typescript::TypeScriptBackend, Backend,
};
use redwood::config::{
    self, CliConfig, GeneratorConfig, GoConfig, PythonConfig, RubyConfig, TypeScriptConfig,
};
use redwood::{ir, openapi};

/// redwood: a statically-typed, dependency-less SDK generator.
///
/// Reads an OpenAPI spec into a normalized IR and projects it into
/// production-ready SDKs, docs, and CLIs.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// OpenAPI spec (YAML or JSON): a filesystem path or an http(s) URL.
    #[arg(long)]
    spec: String,

    /// Target to generate.
    #[arg(long, value_enum)]
    language: Language,

    /// Spec-wide policy config (redwood.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Language-specific config (e.g. typescript.config.toml).
    #[arg(long)]
    lang_config: Option<PathBuf>,

    /// Output directory. Defaults to ./gen/<language>.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Language {
    Typescript,
    Go,
    Python,
    Ruby,
    Cli,
    Docs,
    Manifest,
    Openapi,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let source = redwood::input::read_spec(&cli.spec)?;
    let spec = openapi::parse(&source).context("parsing OpenAPI spec")?;
    let generator_config: GeneratorConfig = config::load(cli.config.as_deref())?;

    let mut api = ir::lower::lower(&spec).context("lowering spec to IR")?;
    config::apply(&mut api, &generator_config)?;

    // Per-language config comes from redwood.toml's [lang.*] sections; an
    // explicit --lang-config file still overrides for back-compat.
    let lang = generator_config.lang;
    let backend: Box<dyn Backend> = match cli.language {
        Language::Typescript => Box::new(TypeScriptBackend {
            config: match cli.lang_config.as_deref() {
                Some(path) => config::load::<TypeScriptConfig>(Some(path))?,
                None => lang.typescript,
            },
        }),
        Language::Go => Box::new(GoBackend {
            config: match cli.lang_config.as_deref() {
                Some(path) => config::load::<GoConfig>(Some(path))?,
                None => lang.go,
            },
        }),
        Language::Python => Box::new(PythonBackend {
            config: match cli.lang_config.as_deref() {
                Some(path) => config::load::<PythonConfig>(Some(path))?,
                None => lang.python,
            },
        }),
        Language::Ruby => Box::new(RubyBackend {
            config: match cli.lang_config.as_deref() {
                Some(path) => config::load::<RubyConfig>(Some(path))?,
                None => lang.ruby,
            },
        }),
        Language::Cli => Box::new(CliBackend {
            config: match cli.lang_config.as_deref() {
                Some(path) => config::load::<CliConfig>(Some(path))?,
                None => lang.cli,
            },
        }),
        Language::Docs => Box::new(DocsBackend),
        Language::Manifest => Box::new(ManifestBackend),
        Language::Openapi => Box::new(OpenApiBackend {
            spec_source: source.clone(),
            ts_config: lang.typescript,
            go_config: lang.go,
            py_config: lang.python,
            rb_config: lang.ruby,
            cli_config: lang.cli,
        }),
    };

    // Each language backend owns its docs (native README + api.md rendered
    // through its own formatters); the manifest target ships none, and the
    // docs target is the generic reference alone.
    let files = backend.generate(&api)?;

    let out_dir = cli
        .out
        .unwrap_or_else(|| PathBuf::from("gen").join(backend.name()));
    let count = files.len();
    let format_go = matches!(backend.name(), "go" | "cli");
    redwood::output::write(&out_dir, &files, format_go)?;
    eprintln!(
        "generated {count} files for {} v{} -> {}",
        api.name,
        api.version,
        out_dir.display()
    );
    Ok(())
}
