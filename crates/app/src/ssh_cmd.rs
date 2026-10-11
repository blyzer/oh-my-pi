//! Authoritative scoped native SSH configuration and standalone client.

use std::{
	env,
	path::{Path, PathBuf},
};

use clap::{Args, Subcommand, ValueEnum};
use miette::IntoDiagnostic as _;
use omp_core::{Str, project_file::Containment};
use omp_envd::ssh::{AuthPolicy, HostConfig, HostPaths, HostStore, SshService};

/// Native SSH command options.
#[derive(Clone, Debug, Args)]
pub struct SshArgs {
	/// Configuration and client operation.
	#[command(subcommand)]
	pub command: SshCommand,
}

/// Writable native SSH configuration scope.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum SshScope {
	/// Repository-local `.omp/hosts.toml`.
	#[default]
	Project,
	/// User configuration root `hosts.toml` (`~/.o2`, profile-aware).
	User,
}

/// Native SSH configuration and bounded client operations.
#[derive(Clone, Debug, Subcommand)]
pub enum SshCommand {
	/// List configured host aliases.
	List {
		/// Restrict inventory to one scope.
		#[arg(long, value_enum)]
		scope: Option<SshScope>,
	},
	/// Add or replace one configured host.
	Add {
		/// Stable configured alias.
		alias:    Str,
		/// DNS name or numeric address.
		#[arg(long)]
		host:     Str,
		/// Remote account name.
		#[arg(long)]
		user:     Str,
		/// SSH port.
		#[arg(long, default_value_t = 22)]
		port:     u16,
		/// Pinned SHA-256 server host-key fingerprint.
		#[arg(long = "host-key")]
		host_key: Str,
		/// Unencrypted private-key path; omission uses the native SSH agent.
		#[arg(long)]
		key:      Option<PathBuf>,
		/// Writable configuration scope.
		#[arg(long, value_enum, default_value_t = SshScope::Project)]
		scope:    SshScope,
	},
	/// Remove one configured host.
	Remove {
		/// Stable configured alias.
		alias: Str,
		/// Writable configuration scope.
		#[arg(long, value_enum, default_value_t = SshScope::Project)]
		scope: SshScope,
	},
	/// Probe pinned-host-key authentication and SFTP support.
	Probe {
		/// Configured host alias to authenticate and inspect.
		alias: Str,
	},
	/// Execute one bounded remote command.
	Exec {
		/// Configured host alias on which to run the command.
		alias:   Str,
		#[arg(trailing_var_arg = true, required = true)]
		/// Executable and arguments passed verbatim to the remote process.
		command: Vec<Str>,
	},
}

/// Runs scoped writer and bounded native transport operations.
pub async fn run(args: SshArgs) -> miette::Result<()> {
	run_in(args, &host_paths()?).await
}

/// [`run`] against an explicit `hosts.toml` pair.
async fn run_in(args: SshArgs, paths: &HostPaths) -> miette::Result<()> {
	match args.command {
		SshCommand::List { scope } => {
			for (label, path, containment) in scoped_paths(scope, paths) {
				let store = HostStore::load(path, containment).into_diagnostic()?;
				for alias in store.aliases() {
					let host = store.get(alias.as_str()).into_diagnostic()?;
					println!("{label}\t{}\t{}@{}:{}", alias, host.user, host.address, host.port);
				}
			}
			Ok(())
		},
		SshCommand::Add { alias, host, user: remote_user, port, host_key, key, scope } => {
			let (path, containment) = scope_path(scope, paths);
			let store = HostStore::load(path, containment).into_diagnostic()?;
			store
				.upsert(path, alias.clone(), HostConfig {
					address: host,
					port,
					user: remote_user,
					host_key,
					auth: key.map_or(AuthPolicy::Agent, |path| AuthPolicy::Key { path }),
					timeout_secs: 30,
				})
				.into_diagnostic()?;
			println!("configured SSH host `{alias}` in {}", path.display());
			Ok(())
		},
		SshCommand::Remove { alias, scope } => {
			let (path, containment) = scope_path(scope, paths);
			let removed = HostStore::load(path, containment)
				.into_diagnostic()?
				.remove(path, alias.as_str())
				.into_diagnostic()?;
			if !removed {
				return Err(miette::miette!(
					"SSH host `{alias}` is not configured in {}",
					path.display()
				));
			}
			println!("removed SSH host `{alias}` from {}", path.display());
			Ok(())
		},
		SshCommand::Probe { alias } => {
			let service = service(alias.as_str(), paths)?;
			let caps = service.probe(alias.as_str()).await.into_diagnostic()?;
			println!("{}: exec={} sftp={}", alias, caps.exec, caps.sftp);
			Ok(())
		},
		SshCommand::Exec { alias, command } => {
			let service = service(alias.as_str(), paths)?;
			let command = command
				.iter()
				.map(Str::as_str)
				.collect::<Vec<_>>()
				.join(" ");
			let output = service
				.exec(alias.as_str(), &command, 1024 * 1024)
				.await
				.into_diagnostic()?;
			print!("{}", String::from_utf8_lossy(output.stdout.as_ref()));
			eprint!("{}", String::from_utf8_lossy(output.stderr.as_ref()));
			if output.exit_status.unwrap_or_default() != 0 {
				return Err(miette::miette!(
					"remote command exited with status {}",
					output.exit_status.unwrap_or_default()
				));
			}
			Ok(())
		},
	}
}

/// The `hosts.toml` pair for the current working directory: project
/// `.omp/hosts.toml` over the user configuration root (`~/.o2`).
pub(crate) fn host_paths() -> miette::Result<HostPaths> {
	let user_root = omp_core::dirs::user_config_root().into_diagnostic()?;
	let project_root = env::current_dir().into_diagnostic()?;
	Ok(HostPaths::new(&user_root, &project_root))
}

/// Opens the effective host authority (project aliases shadow user ones) and
/// verifies `alias` is configured.
pub(crate) fn service(alias: &str, paths: &HostPaths) -> miette::Result<SshService> {
	let store = HostStore::load_layered(paths).into_diagnostic()?;
	store.get(alias).into_diagnostic()?;
	Ok(SshService::new(store))
}

/// The file one scope writes and how it is read: the project file must
/// resolve inside the repository, so a `.omp` linked outside is refused
/// rather than written through.
fn scope_path(scope: SshScope, paths: &HostPaths) -> (&Path, Containment<'_>) {
	match scope {
		SshScope::Project => (&paths.project, Containment::Within(&paths.root)),
		SshScope::User => (&paths.user, Containment::Unconfined),
	}
}

fn scoped_paths(
	scope: Option<SshScope>,
	paths: &HostPaths,
) -> Vec<(&'static str, &Path, Containment<'_>)> {
	let entry = |label, scope| {
		let (path, containment) = scope_path(scope, paths);
		(label, path, containment)
	};
	match scope {
		Some(SshScope::Project) => vec![entry("project", SshScope::Project)],
		Some(SshScope::User) => vec![entry("user", SshScope::User)],
		None => vec![entry("project", SshScope::Project), entry("user", SshScope::User)],
	}
}

#[cfg(all(test, unix))]
mod tests {
	use std::{fs, os::unix::fs::symlink};

	use clap::Parser as _;

	use super::*;

	#[derive(clap::Parser)]
	struct Cli {
		#[command(flatten)]
		args: SshArgs,
	}

	/// `omp ssh add` defaults to the project scope: in a repository whose
	/// `.omp` links outside it, the add is refused and writes nothing there.
	#[tokio::test]
	async fn ssh_add_refuses_a_project_omp_linked_outside() {
		let temp = tempfile::tempdir().expect("tempdir");
		let project = temp.path().join("project");
		let outside = temp.path().join("outside");
		fs::create_dir_all(project.join(".git")).expect("repository marker");
		fs::create_dir_all(&outside).expect("outside directory");
		symlink(&outside, project.join(".omp")).expect("outside .omp link");
		let paths = HostPaths::new(&temp.path().join("o2"), &project);
		let add = Cli::parse_from([
			"ssh",
			"add",
			"build",
			"--host",
			"build.example",
			"--user",
			"ci",
			"--host-key",
			"SHA256:test",
		]);

		let error = run_in(add.args, &paths)
			.await
			.expect_err("an outside .omp is refused");
		assert!(error.to_string().contains("outside the project root"), "{error}");
		assert!(
			fs::read_dir(&outside)
				.expect("outside listing")
				.next()
				.is_none(),
			"nothing was written outside the repository"
		);
	}
}
