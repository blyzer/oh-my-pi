use std::{
	io::{self, Write},
	path::PathBuf,
};

use clap::{Args, Subcommand};
use miette::{IntoDiagnostic as _, miette};
use omp_kb::store::Store;
use serde::Serialize;

#[derive(Clone, Debug, Args)]
pub struct KbArgs {
	#[arg(long, default_value = ".")]
	pub repo:    PathBuf,
	#[command(subcommand)]
	pub command: KbCommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum KbCommand {
	Update,
	Query {
		symbol: String,
		#[arg(long)]
		live:   bool,
	},
	Refresh {
		#[arg(required = true)]
		paths: Vec<PathBuf>,
	},
}

#[derive(Serialize)]
struct Updated<'a> {
	revision:    &'a str,
	fingerprint: &'a str,
}

#[derive(Serialize)]
struct Scheduled {
	scheduled: bool,
}

fn emit(value: &impl Serialize) -> miette::Result<()> {
	let stdout = io::stdout();
	let mut out = stdout.lock();
	serde_json::to_writer(&mut out, value).into_diagnostic()?;
	writeln!(out).into_diagnostic()?;
	Ok(())
}

pub fn run(args: KbArgs) -> miette::Result<()> {
	let store = Store::open(&args.repo).map_err(|error| miette!("{error:#}"))?;
	match args.command {
		KbCommand::Update => {
			let index = store.update().map_err(|error| miette!("{error:#}"))?;
			emit(&Updated {
				revision:    &index.snapshot.revision,
				fingerprint: &index.snapshot.fingerprint,
			})
		},
		KbCommand::Query { symbol, live } => {
			let index = if live {
				store.build().map_err(|error| miette!("{error:#}"))?
			} else {
				store.load().map_err(|error| miette!("{error:#}"))?
			};
			let stale = if live {
				false
			} else {
				store.stale(&index).map_err(|error| miette!("{error:#}"))?
			};
			emit(&index.query(&symbol, if live { "live" } else { "indexed" }, stale))
		},
		KbCommand::Refresh { paths } => {
			store
				.refresh(&paths)
				.map_err(|error| miette!("{error:#}"))?;
			emit(&Scheduled { scheduled: true })
		},
	}
}
