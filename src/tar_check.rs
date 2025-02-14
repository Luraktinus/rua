use crate::terminal_util;

use colored::*;
use log::debug;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use tar::*;
use xz2::read::XzDecoder;

pub fn tar_check_unwrap(tar_file: &Path) {
	let result = tar_check(tar_file);
	result.unwrap_or_else(|err| {
		eprintln!("{}", err);
		std::process::exit(1)
	})
}

pub fn tar_check(tar_file: &Path) -> Result<(), String> {
	let tar_str = tar_file
		.to_str()
		.unwrap_or_else(|| panic!("{}:{} Failed to parse tar file name", file!(), line!()));
	let archive = File::open(&tar_file).unwrap_or_else(|_| panic!("cannot open file {}", tar_str));
	if tar_str.ends_with(".tar.xz") {
		tar_check_archive(Archive::new(XzDecoder::new(archive)), tar_str);
		debug!("Checked package tar file {}", tar_str);
		Ok(())
	} else if tar_str.ends_with(".tar") {
		tar_check_archive(Archive::new(archive), tar_str);
		debug!("Checked package tar file {}", tar_str);
		Ok(())
	} else {
		Err(format!(
			"Archive {:?} cannot be analyzed. Only .tar.xz and .tar files are supported",
			tar_file
		))
	}
}

fn tar_check_archive<R: Read>(mut archive: Archive<R>, path_str: &str) {
	let mut install_file = String::new();
	let mut all_files = Vec::new();
	let mut executable_files = Vec::new();
	let mut suid_files = Vec::new();
	let archive_files = archive
		.entries()
		.unwrap_or_else(|e| panic!("cannot open archive {}, {}", path_str, e));
	for file in archive_files {
		let mut file =
			file.unwrap_or_else(|e| panic!("cannot access tar file in {}, {}", path_str, e));
		let path = {
			let path = file.header().path().unwrap_or_else(|e| {
				panic!(
					"Failed to extract tar file metadata for file in {}, {}",
					path_str, e,
				)
			});
			path.to_str()
				.unwrap_or_else(|| panic!("{}:{} failed to parse file name", file!(), line!()))
				.to_owned()
		};
		let mode = file.header().mode().unwrap_or_else(|_| {
			panic!(
				"{}:{} Failed to get file mode for file {}",
				file!(),
				line!(),
				path
			)
		});
		let is_normal = !path.ends_with('/') && !path.starts_with('.');
		if is_normal {
			all_files.push(path.clone());
		}
		if is_normal && (mode & 0o111 > 0) {
			executable_files.push(path.clone());
		}
		if mode > 0o777 {
			suid_files.push(path.clone());
		}
		if &path == ".INSTALL" {
			file.read_to_string(&mut install_file).unwrap_or_else(|_| {
				panic!("Failed to read INSTALL script from tar file {}", path_str)
			});
		}
	}

	let has_install = !install_file.is_empty();
	loop {
		if suid_files.is_empty() {
			eprint!("Package {} has no SUID files.\n", path_str);
		}
		eprint!(
			"[E]=list executable files, [L]=list all files, \
			 [T]=run shell to inspect, "
		);
		if has_install {
			eprint!("[I]=show install file, ");
		};
		if !suid_files.is_empty() {
			eprint!("{}", "!!! [S]=list SUID files!!!, ".red())
		};
		eprint!("[O]=ok, proceed. ");
		let string = terminal_util::read_line_lowercase();
		eprintln!();
		if string == "s" && !suid_files.is_empty() {
			for path in &suid_files {
				eprintln!("{}", path);
			}
		} else if string == "e" {
			for path in &executable_files {
				eprintln!("{}", path);
			}
		} else if string == "l" {
			for path in &all_files {
				eprintln!("{}", path);
			}
		} else if string == "i" && has_install {
			eprintln!("{}", &install_file);
		} else if string == "t" {
			let dir = PathBuf::from(path_str);
			let dir = dir.parent().unwrap_or_else(|| Path::new("."));
			eprintln!("Exit the shell with `logout` or Ctrl-D...");
			terminal_util::run_env_command(&dir.to_path_buf(), "SHELL", "bash", &[]);
		} else if string == "o" {
			break;
		} else if string == "q" {
			eprintln!("Exiting...");
			std::process::exit(-1);
		}
	}
}
