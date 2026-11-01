#![doc = include_str!("../README.md")]

use std::{
	io::{self, Write},
	path::PathBuf,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use omp_kb::store::Store;
use serde::Serialize;

#[derive(Parser)]
#[command(name = "omp-kb", about = "Query a revision-aware Rust SCIP index")]
struct Cli {
	#[arg(long, global = true, default_value = ".")]
	repo:    PathBuf,
	#[command(subcommand)]
	command: Action,
}

#[derive(Subcommand)]
enum Action {
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

fn output(value: &impl Serialize) -> Result<()> {
	let stdout = io::stdout();
	let mut out = stdout.lock();
	serde_json::to_writer(&mut out, value)?;
	writeln!(out)?;
	Ok(())
}

fn run() -> Result<()> {
	let cli = Cli::parse();
	let store = Store::open(&cli.repo)?;
	match cli.command {
		Action::Update => {
			let index = store.update()?;
			output(&Updated {
				revision:    &index.snapshot.revision,
				fingerprint: &index.snapshot.fingerprint,
			})
		},
		Action::Query { symbol, live } => {
			let index = if live { store.build()? } else { store.load()? };
			let stale = if live { false } else { store.stale(&index)? };
			output(&index.query(&symbol, if live { "live" } else { "indexed" }, stale))
		},
		Action::Refresh { paths } => {
			store.refresh(&paths)?;
			output(&Scheduled { scheduled: true })
		},
	}
}

fn main() {
	if let Err(error) = run() {
		eprintln!("{error:#}");
		std::process::exit(1);
	}
}
