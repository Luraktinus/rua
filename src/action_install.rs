use crate::aur_rpc_utils;
use crate::pacman;
use crate::reviewing;
use crate::rua_files;
use crate::tar_check;
use crate::terminal_util;
use crate::wrapped;

use directories::ProjectDirs;
use fs_extra::dir::CopyOptions;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;
use log::debug;
use log::trace;
use raur::Package;
use std::collections::HashSet;
use std::fs;
use std::fs::ReadDir;
use std::path::PathBuf;

pub fn install(targets: &[String], dirs: &ProjectDirs, is_offline: bool, asdeps: bool) {
	let alpm = pacman::create_alpm();
	let (split_to_raur, pacman_deps, split_to_depth) =
		aur_rpc_utils::recursive_info(targets, &alpm).unwrap_or_else(|err| {
			panic!("Failed to fetch info from AUR, {}", err);
		});
	let split_to_pkgbase: IndexMap<String, String> = split_to_raur
		.iter()
		.map(|(split, raur)| (split.to_string(), raur.package_base.to_string()))
		.collect();
	let split_to_version: IndexMap<String, String> = split_to_raur
		.iter()
		.map(|(split, raur)| (split.to_string(), raur.version.to_string()))
		.collect();

	let not_found = split_to_depth
		.keys()
		.filter(|pkg| !split_to_raur.contains_key(*pkg))
		.collect_vec();
	if !not_found.is_empty() {
		eprintln!(
			"Need to install packages: {:?}, but they are not found on AUR.",
			not_found
		);
		std::process::exit(1)
	}

	show_install_summary(&pacman_deps, &split_to_depth);
	for pkgbase in split_to_pkgbase.values().collect::<HashSet<_>>() {
		let dir = rua_files::review_dir(dirs, pkgbase);
		fs::create_dir_all(&dir).unwrap_or_else(|err| {
			panic!("Failed to create repository dir for {}, {}", pkgbase, err)
		});
		reviewing::review_repo(&dir, pkgbase, dirs);
	}
	pacman::ensure_pacman_packages_installed(pacman_deps);
	install_all(
		dirs,
		split_to_depth,
		split_to_pkgbase,
		split_to_version,
		is_offline,
		asdeps,
	);
}

fn show_install_summary(pacman_deps: &IndexSet<String>, aur_packages: &IndexMap<String, i32>) {
	if pacman_deps.len() + aur_packages.len() == 1 {
		return;
	}
	eprintln!("\nIn order to install all targets, the following pacman packages will need to be installed:");
	eprintln!(
		"{}",
		pacman_deps.iter().map(|s| format!("  {}", s)).join("\n")
	);
	eprintln!("And the following AUR packages will need to be built and installed:");
	let mut aur_packages = aur_packages.iter().collect::<Vec<_>>();
	aur_packages.sort_by_key(|pair| -*pair.1);
	for (aur, dep) in &aur_packages {
		debug!("depth {}: {}", dep, aur);
	}
	eprintln!(
		"{}\n",
		aur_packages.iter().map(|s| format!("  {}", s.0)).join("\n")
	);
	loop {
		eprint!("Proceed? [O]=ok, Ctrl-C=abort. ");
		let string = terminal_util::read_line_lowercase();
		if string == "o" {
			break;
		}
	}
}

fn install_all(
	dirs: &ProjectDirs,
	split_to_depth: IndexMap<String, i32>,
	split_to_pkgbase: IndexMap<String, String>,
	split_to_version: IndexMap<String, String>,
	offline: bool,
	asdeps: bool,
) {
	let archive_whitelist = split_to_version
		.into_iter()
		.map(|pair| format!("{}-{}", pair.0, pair.1))
		.collect::<Vec<_>>();
	trace!("All expected archive files: {:?}", archive_whitelist);
	// get a list of (pkgbase, depth)
	let packages = split_to_pkgbase.iter().map(|(split, pkgbase)| {
		let depth = split_to_depth
			.get(split)
			.expect("Internal error: split package doesn't have recursive depth");
		(pkgbase.to_string(), *depth, split.to_string())
	});
	// sort pairs in descending depth order
	let packages = packages.sorted_by_key(|(_pkgbase, depth, _split)| -depth);
	// Note that a pkgbase can appear at multiple depths because
	// multiple split pkgnames can be at multiple depths.
	// In this case, we only take the first occurrence of pkgbase,
	// which would be the maximum depth because of sort order.
	// We only take one occurrence because we want the package to only be built once.
	let packages: Vec<(String, i32, String)> = packages
		.unique_by(|(pkgbase, _depth, _split)| pkgbase.to_string())
		.collect::<Vec<_>>();
	// once we have a collection of pkgname-s and their depth, proceed straightforwardly.
	for (depth, packages) in &packages.iter().group_by(|(_pkgbase, depth, _split)| *depth) {
		let packages = packages.collect::<Vec<&(String, i32, String)>>();
		for (pkgbase, _depth, _split) in &packages {
			let review_dir = rua_files::review_dir(dirs, pkgbase);
			let build_dir = rua_files::build_dir(dirs, pkgbase);
			rm_rf::force_remove_all(&build_dir).expect("Failed to remove old build dir");
			std::fs::create_dir_all(&build_dir).expect("Failed to create build dir");
			fs_extra::copy_items(
				&vec![review_dir],
				rua_files::global_build_dir(dirs),
				&CopyOptions::new(),
			)
			.expect("failed to copy reviewed dir to build dir");
			rm_rf::force_remove_all(build_dir.join(".git")).expect("Failed to remove .git");
			wrapped::build_directory(
				&build_dir.to_str().expect("Non-UTF8 directory name"),
				dirs,
				offline,
			);
		}
		for (pkgbase, _depth, _split) in &packages {
			check_tars_and_move(pkgbase, dirs, &archive_whitelist);
		}
		// This relation between split_name and the archive file is not actually correct here.
		// Instead, all archive files of some group will be bound to one split name only here.
		// This is probably still good enough for install verification though --
		// and we only use this relation for this purpose. Feel free to improve, if you want...
		let mut files_to_install: Vec<(String, PathBuf)> = Vec::new();
		for (pkgbase, _depth, split) in &packages {
			let checked_tars = rua_files::checked_tars_dir(dirs, &pkgbase);
			let read_dir_iterator = fs::read_dir(checked_tars).unwrap_or_else(|e| {
				panic!(
					"Failed to read 'checked_tars' directory for {}, {}",
					pkgbase, e
				)
			});

			for file in read_dir_iterator {
				files_to_install.push((
					split.to_string(),
					file.expect("Failed to access checked_tars dir").path(),
				));
			}
		}
		pacman::ensure_aur_packages_installed(files_to_install, asdeps || depth > 0);
	}
}

pub fn check_tars_and_move(name: &str, dirs: &ProjectDirs, archive_whitelist: &[String]) {
	debug!("{}:{} checking tars for package {}", file!(), line!(), name);
	let build_dir = rua_files::build_dir(dirs, name);
	let dir_items: ReadDir = build_dir.read_dir().unwrap_or_else(|err| {
		panic!(
			"Failed to read directory contents for {:?}, {}",
			&build_dir, err
		)
	});
	let dir_items = dir_items.map(|f| f.expect("Failed to open file for tar_check analysis"));
	let dir_items = dir_items
		.filter(|file| {
			let file_name = file.file_name();
			let file_name = file_name
				.to_str()
				.expect("Non-UTF8 characters in tar file name");
			archive_whitelist
				.iter()
				.any(|prefix| file_name.starts_with(prefix))
		})
		.collect::<Vec<_>>();
	trace!("Files filtered for tar checking: {:?}", &dir_items);
	for file in dir_items.iter() {
		tar_check::tar_check_unwrap(&file.path());
	}
	debug!("all package (tar) files checked, moving them");
	let checked_tars_dir = rua_files::checked_tars_dir(dirs, name);
	rm_rf::force_remove_all(&checked_tars_dir).unwrap_or_else(|err| {
		panic!(
			"Failed to clean checked tar files dir {:?}, {}",
			checked_tars_dir, err,
		)
	});
	fs::create_dir_all(&checked_tars_dir).unwrap_or_else(|err| {
		panic!(
			"Failed to create checked_tars dir {:?}, {}",
			&checked_tars_dir, err
		);
	});

	for file in dir_items {
		let file_name = file.file_name();
		let file_name = file_name
			.to_str()
			.expect("Non-UTF8 characters in tar file name");
		fs::rename(&file.path(), checked_tars_dir.join(file_name)).unwrap_or_else(|e| {
			panic!(
				"Failed to move {:?} (build artifact) to {:?}, {}",
				&file, &checked_tars_dir, e,
			)
		});
	}
}

pub fn raur_info(pkg: &str) -> Option<Package> {
	trace!(
		"{}:{} Fetching AUR information for package {}",
		file!(),
		line!(),
		pkg
	);
	let info = raur::info(&[pkg]);
	let info = info.unwrap_or_else(|e| panic!("Failed to fetch info for package {}, {}", &pkg, e));
	info.into_iter().next()
}
