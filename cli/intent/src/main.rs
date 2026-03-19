// main.rs
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod check;

#[derive(Parser)]
#[command(name = "wa2", version, about = "WA2 - Well-Architected 2")]
struct Cli {
	#[command(subcommand)]
	command: Commands,
}

#[derive(Subcommand)]
enum Commands {
	#[command(
		about = "Evaluate a target system against an intent profile",
		long_about = "Evaluate a CloudFormation template against a WA2 intent profile.\n\nProduces structured results and optional graph output."
	)]
	Check {
		#[arg(
			long,
			value_name = "NAME",
			help = "Intent profile name to evaluate",
			long_help = "Intent profile name to evaluate.\n\nThis selects the profile inside the loaded framework and optional entry intent."
		)]
		profile: String,

		#[arg(
			long,
			value_name = "FILE",
			help = "Path to the CloudFormation template",
			long_help = "Path to the CloudFormation template to analyse.\n\nSupported formats are YAML and JSON."
		)]
		target: PathBuf,

		#[arg(
			long,
			value_name = "FILE",
			help = "Optional intent entry file",
			long_help = "Optional intent entry file to layer on top of the framework.\n\nWhen omitted, the default framework/quickstart behavior is used."
		)]
		entry: Option<PathBuf>,

		#[arg(
			long,
			help = "Show the projected model graph",
			long_help = "Show the projected and derived model graph after analysis.\n\nThis is mainly intended for debugging."
		)]
		graph: bool,

		#[arg(
			long,
			help = "Disable CloudFormation validation",
			long_help = "Disable the separate CloudFormation validation stage.\n\nBy default validation runs concurrently and is reported after results."
		)]
		novalidation: bool,
	},
}

#[tokio::main]
async fn main() -> ExitCode {
	let cli = Cli::parse();

	match cli.command {
		Commands::Check {
			profile,
			target,
			entry,
			graph,
			novalidation,
		} => check::run(&profile, &target, entry.as_deref(), graph, !novalidation).await,
	}
}
