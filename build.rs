use std::process::{Command, ExitStatus, Output};

#[cfg(not(target_os = "windows"))]
use std::os::unix::process::ExitStatusExt;

#[cfg(target_os = "windows")]
use std::os::windows::process::ExitStatusExt;

fn main() {
	println!("cargo:rerun-if-changed=src/");
	// Rebuild when static assets change, and stamp each build so asset URLs
	// (style.css?v=...) bust browser caches automatically.
	println!("cargo:rerun-if-changed=static/");
	let stamp = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs())
		.unwrap_or_default();
	println!("cargo:rustc-env=REDLIB_BUILD_STAMP={stamp}");
	let output = String::from_utf8(
		Command::new("git")
			.args(["rev-parse", "HEAD"])
			.output()
			.unwrap_or(Output {
				stdout: vec![],
				stderr: vec![],
				status: ExitStatus::from_raw(0),
			})
			.stdout,
	)
	.unwrap_or_default();
	let git_hash = if output == String::default() { "dev".into() } else { output };
	println!("cargo:rustc-env=GIT_HASH={git_hash}");
}
