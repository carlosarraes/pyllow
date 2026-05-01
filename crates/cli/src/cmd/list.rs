use crate::report::Format;
use anyhow::{anyhow, Context, Result};
use colored::Colorize;
use pyllow_analyzer::collect_inventory;
use pyllow_types::{EntryPointSource, Inventory};
use std::path::PathBuf;
use tabled::{builder::Builder, settings::Style};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum What {
    All,
    EntryPoints,
    Files,
    Plugins,
}

pub fn run(what: What, path: PathBuf, format: Format) -> Result<()> {
    let (config, _root) = super::load_config(&path)?;
    let inventory = collect_inventory(&config).context("collecting inventory")?;
    // `list` is an inventory dump, not an issue report — SARIF is a
    // code-scanning result format with no notion of inventory entries, and a
    // Markdown renderer doesn't exist yet. Previously both silently emitted
    // JSON, so any script relying on the requested type got the wrong
    // document. Fail loudly until they're either supported or removed from
    // the global format enum.
    match (what, format) {
        (_, Format::Sarif) => {
            return Err(anyhow!(
                "`pyllow list --format sarif` is not supported (SARIF is for issue results, not inventories); use --format human or json"
            ));
        }
        (_, Format::Markdown) => {
            return Err(anyhow!(
                "`pyllow list --format markdown` is not implemented yet; use --format human or json"
            ));
        }
        (What::All, Format::Json) => print_json_all(&inventory),
        (What::All, Format::Human) => print_human_all(&inventory),
        (w, Format::Json) => print_json_section(w, &inventory),
        (w, Format::Human) => print_human_section(w, &inventory),
    }
    Ok(())
}

fn print_json_section(what: What, inv: &Inventory) {
    let value = match what {
        What::EntryPoints => serde_json::to_string_pretty(&inv.entry_points),
        What::Files => serde_json::to_string_pretty(&inv.files),
        What::Plugins => serde_json::to_string_pretty(&inv.plugins_run),
        What::All => serde_json::to_string_pretty(inv),
    };
    match value {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing: {e}"),
    }
}

fn print_json_all(inv: &Inventory) {
    match serde_json::to_string_pretty(inv) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing: {e}"),
    }
}

fn print_human_section(what: What, inv: &Inventory) {
    match what {
        What::EntryPoints => print_entry_points(inv),
        What::Files => print_files(inv),
        What::Plugins => print_plugins(inv),
        What::All => print_human_all(inv),
    }
}

fn print_human_all(inv: &Inventory) {
    println!("{}", "## entry points".bold());
    print_entry_points(inv);
    println!();
    println!("{}", "## files".bold());
    print_files(inv);
    println!();
    println!("{}", "## plugins".bold());
    print_plugins(inv);
}

fn print_entry_points(inv: &Inventory) {
    // A file can be claimed by multiple sources (e.g. plugin:script +
    // plugin:pydantic both match a __main__-style entrypoint script).
    // Group by path so the human view shows one row per file with the
    // sources comma-joined. JSON/SARIF callers still see the raw list.
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<&std::path::Path, (String, Vec<String>)> = BTreeMap::new();
    for ep in &inv.entry_points {
        let entry = grouped
            .entry(&ep.path)
            .or_insert_with(|| (ep.dotted_module.clone(), Vec::new()));
        entry.1.push(source_label(&ep.source));
    }

    let mut b = Builder::new();
    b.push_record(["path", "module", "sources"]);
    for (path, (module, sources)) in &grouped {
        b.push_record([
            path.display().to_string(),
            module.clone(),
            sources.join(", "),
        ]);
    }
    print_table(
        b,
        format!(
            "{} entry points ({} unique paths)",
            inv.entry_points.len(),
            grouped.len()
        ),
    );
}

fn print_files(inv: &Inventory) {
    let mut b = Builder::new();
    b.push_record(["path", "module", "kind"]);
    for f in &inv.files {
        b.push_record([
            f.path.display().to_string(),
            f.dotted_module.clone(),
            format!("{:?}", f.kind).to_lowercase(),
        ]);
    }
    print_table(b, format!("{} files", inv.files.len()));
}

fn print_plugins(inv: &Inventory) {
    let mut b = Builder::new();
    b.push_record(["plugin"]);
    for p in &inv.plugins_run {
        b.push_record([p.clone()]);
    }
    print_table(b, format!("{} plugins active", inv.plugins_run.len()));
}

fn print_table(b: Builder, footer: String) {
    let mut t = b.build();
    t.with(Style::rounded());
    println!("{t}");
    println!("{footer}");
}

fn source_label(s: &EntryPointSource) -> String {
    match s {
        EntryPointSource::Config => "config".to_string(),
        EntryPointSource::Plugin(name) => format!("plugin:{name}"),
        EntryPointSource::ScriptEntryPoint => "script".to_string(),
        EntryPointSource::ModuleGetattr => "module-getattr".to_string(),
        EntryPointSource::PyprojectEntryPoint(group) => format!("pyproject:{group}"),
    }
}
