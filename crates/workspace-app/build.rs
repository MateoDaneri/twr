#[cfg(not(feature = "ghostty"))]
fn main() {}

#[cfg(feature = "ghostty")]
fn main() {
    use sha2::{Digest, Sha256};
    use std::{env, fs, path::PathBuf};
    const COMMIT: &str = "8ef0ec1238220a5a714be452ed2844f0691fcf83";
    println!("cargo:rerun-if-env-changed=TERMINAL_WORKSPACE_GHOSTTY_STAGE");
    println!("cargo:rerun-if-changed=native/ghostty_bridge.c");
    println!("cargo:rerun-if-changed=native/ghostty_bridge.h");
    assert_eq!(
        env::var("TARGET").unwrap(),
        "aarch64-apple-darwin",
        "The evaluated core is macOS arm64 only"
    );
    let supplied = PathBuf::from(
        env::var("TERMINAL_WORKSPACE_GHOSTTY_STAGE")
            .expect("Explicit prepared Ghostty stage required"),
    );
    let stage = supplied.canonicalize().expect("Ghostty stage unavailable");
    assert!(
        stage.parent() == Some(std::path::Path::new("/private/tmp"))
            && stage
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tw-ghostty-probe-")
    );
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::metadata(&stage).unwrap();
    assert!(
        meta.permissions().mode() & 0o077 == 0,
        "Ghostty stage must be private"
    );
    let local_owner = fs::metadata(env::var("OUT_DIR").unwrap()).unwrap().uid();
    assert_eq!(
        meta.uid(),
        local_owner,
        "Ghostty stage owner differs from build owner"
    );
    let receipt_path = stage.join("receipt.json");
    assert_eq!(
        receipt_path.canonicalize().unwrap(),
        receipt_path,
        "Receipt symlink rejected"
    );
    assert!(receipt_path.metadata().unwrap().len() < 2 * 1024 * 1024);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["commit"].as_str(), Some(COMMIT));
    let library = stage.join("install/lib/libghostty-vt.a");
    let mut builds: Vec<_> = fs::read_dir(&stage)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("build-")
                && p.extension().is_some_and(|s| s == "json")
        })
        .collect();
    builds.sort();
    let build_path = builds.last().expect("No build receipt");
    assert_eq!(
        build_path.canonicalize().unwrap(),
        *build_path,
        "Build receipt symlink rejected"
    );
    assert!(build_path.metadata().unwrap().len() < 65536);
    let build: serde_json::Value = serde_json::from_slice(&fs::read(build_path).unwrap()).unwrap();
    assert_eq!(build["state"].as_str(), Some("finished"));
    assert_eq!(build["returncode"].as_i64(), Some(0));
    assert!(build["stopped"].is_null() && build["cleanup_errors"].as_array().unwrap().is_empty());
    let expected = build["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["path"].as_str() == library.to_str())
        .expect("Missing library receipt");
    assert_eq!(
        library.canonicalize().unwrap(),
        library,
        "Library symlink rejected"
    );
    assert!(library.metadata().unwrap().len() <= 64 * 1024 * 1024);
    let digest = format!("{:x}", Sha256::digest(fs::read(&library).unwrap()));
    assert_eq!(
        Some(digest.as_str()),
        expected["sha256"].as_str(),
        "Ghostty library changed"
    );
    // Headers must match the exact blobs checked during preparation.
    for (name, expected) in receipt["source_files"]["blobs"].as_object().unwrap() {
        if !name.starts_with("include/") {
            continue;
        }
        let path = stage.join("source").join(name);
        assert!(
            std::path::Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        );
        assert_eq!(
            path.canonicalize().unwrap(),
            path,
            "Header symlink rejected"
        );
        let hash = std::process::Command::new("/usr/bin/git")
            .args(["hash-object", "--no-filters", "--"])
            .arg(&path)
            .current_dir(&stage)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("Cannot verify pinned C header");
        assert!(hash.status.success());
        assert_eq!(
            String::from_utf8(hash.stdout).unwrap().trim(),
            expected.as_str().unwrap(),
            "Header differs from pinned source"
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed={}", receipt_path.display());
    println!("cargo:rerun-if-changed={}", build_path.display());
    println!("cargo:rerun-if-changed={}", library.display());
    cc::Build::new()
        .compiler("/usr/bin/clang")
        .file("native/ghostty_bridge.c")
        .include(stage.join("source/include"))
        .define("GHOSTTY_STATIC", "1")
        .flag("-std=c11")
        .warnings_into_errors(true)
        .compile("terminal_ghostty_bridge");
    // Give the verified archive a distinct name in OUT_DIR. A sibling dylib
    // must never be chosen while linking a standalone app or test executable.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let copied = out.join("libterminal_ghostty_core.a");
    fs::copy(&library, &copied).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&copied).unwrap())),
        digest
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=terminal_ghostty_core");
    println!("cargo:rustc-env=TERMINAL_WORKSPACE_GHOSTTY_COMMIT={COMMIT}");
    println!("cargo:rustc-env=TERMINAL_WORKSPACE_GHOSTTY_ARCHIVE_SHA256={digest}");
}
