use crate::report::Format;
use anyhow::{Context, Result};
use pyllow_analyzer::collect_inventory;
use pyllow_types::{EntryPointSource, Inventory};
use std::path::PathBuf;
use tabled::{builder::Builder, settings::Style};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum What {
    EntryPoints,
    Files,
    Plugins,
}

pub fn run(what: What, path: PathBuf, format: Format) -> Result<()> {
    let (config, _root) = super::load_config(&path)?;
    let inventory = collect_inventory(&config).context("collecting inventory")?;
    match format {
        Format::Json => print_json(what, &inventory),
        Format::Human => print_human(what, &inventory),
    }
    Ok(())
}

fn print_json(what: What, inv: &Inventory) {
    let value = match what {
        What::EntryPoints => serde_json::to_string_pretty(&inv.entry_points),
        What::Files => serde_json::to_string_pretty(&inv.files),
        What::Plugins => serde_json::to_string_pretty(&inv.plugins_run),
    };
    match value {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing: {e}"),
    }
}

fn print_human(what: What, inv: &Inventory) {
    match what {
        What::EntryPoints => {
            let mut b = Builder::new();
            b.push_record(["path", "module", "source"]);
            for ep in &inv.entry_points {
                b.push_record([
                    ep.path.display().to_string(),
                    ep.dotted_module.clone(),
                    source_label(&ep.source),
                ]);
            }
            print_table(b, format!("{} entry points", inv.entry_points.len()));
        }
        What::Files => {
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
        What::Plugins => {
            let mut b = Builder::new();
            b.push_record(["plugin"]);
            for p in &inv.plugins_run {
                b.push_record([p.clone()]);
            }
            print_table(b, format!("{} plugins active", inv.plugins_run.len()));
        }
    }
}

fn print_table(b: Builder, footer: String) {
    let mut t = b.build();
    t.with(Style::rounded());
    println!("{t}");
    println!("{}", footer);
}

fn source_label(s: &EntryPointSource) -> String {
    match s {
        EntryPointSource::Config => "config".to_string(),
        EntryPointSource::Plugin(name) => format!("plugin:{name}"),
        EntryPointSource::ScriptEntryPoint => "script".to_string(),
    }
}
