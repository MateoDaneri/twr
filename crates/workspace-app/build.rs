#[cfg(not(feature = "ghostty"))]
fn main() {}

#[cfg(feature = "ghostty")]
fn main() {
    if std::env::var_os("TERMINAL_WORKSPACE_GHOSTTY_RELEASE_STAGE").is_some() {
        assert!(
            std::env::var_os("TERMINAL_WORKSPACE_GHOSTTY_STAGE").is_none(),
            "Choose one Ghostty source"
        );
        official_release_main();
        return;
    }
    println!("cargo:rerun-if-env-changed=TERMINAL_WORKSPACE_GHOSTTY_RELEASE_STAGE");
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
        assert!(std::path::Path::new(name)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_))));
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

// Hashes derived from the upstream 1.0.0 XCFramework whose minisign
// signatures were verified against the vendor's published key.
#[cfg(feature = "ghostty")]
fn official_release_main() {
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeSet,
        env, fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };
    const COMMIT: &str = "0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec";
    const ZIP_HASH: &str = "2c32b7409b2b6c2ef6cfd606526de357762ad458bdc27134ad074e86506a8bc2";
    const LIB_HASH: &str = "497ecbe334670fa194d4c10e50693e0ed3faa925418196960291356962c9c9e5";
    const HEADERS: &[(&str, &str)] = &[
        (
            "ghostty/vt.h",
            "46d95f98dcd73a883fdd7846a627ab1bbed405507a7867b6028728c6290d910b",
        ),
        (
            "ghostty/vt/allocator.h",
            "af1a215712f4fe4a8144837142b307c584e231bf639a5e1f92b13f47814e74a3",
        ),
        (
            "ghostty/vt/build_info.h",
            "440539a7e05c53e4749bff00ce6d2a4ae3aa8e736db21bab783e99a176c857ac",
        ),
        (
            "ghostty/vt/color.h",
            "3afe15f5e2b68b37c460e548470a60be2c510d4ef995f855d57ceaebb7aa8b53",
        ),
        (
            "ghostty/vt/color_scheme.h",
            "9f4688a63f7fdbfaa379314b030c1c6b8c1c8e72570b18a3cd7865c7875e5527",
        ),
        (
            "ghostty/vt/device.h",
            "5c3e2bd5ae2b54470b6b9c2acc23ac5483c495a1d47906ffc3cb1301a798faa5",
        ),
        (
            "ghostty/vt/focus.h",
            "10a964de585b4267f706b4e1dc33a8097094f739cc9a08961ccda4735f01d9d9",
        ),
        (
            "ghostty/vt/formatter.h",
            "6d78c553fbdaaba7958c4c393de9c262a06ac251a63891f67fadc579e33d6085",
        ),
        (
            "ghostty/vt/grid_ref.h",
            "3618e00e9d1637481b0662f327b5363783d6fcc0f1fbd16fc6e0a32247198d31",
        ),
        (
            "ghostty/vt/grid_ref_tracked.h",
            "57cdc4f349de150a944d4dd7b6f17bf604b15d8368563102f577253098482e5a",
        ),
        (
            "ghostty/vt/io.h",
            "276b27886d563ca3f3006eabefd6d2d6b21d3a791f4e5bff3242ac95c9e03dd4",
        ),
        (
            "ghostty/vt/key.h",
            "abd51ebd2eea8ea053f68dfa26759ded280466da6970189d94855a3fd4c343a0",
        ),
        (
            "ghostty/vt/key/encoder.h",
            "7b0c621e6647c80cd91c11072391e019eef2ad927faa5f8fb1e280f2ff330495",
        ),
        (
            "ghostty/vt/key/event.h",
            "4a615f9864ae11bafa06cce7493aa6537804750ed4a6fda8b3f962ea348a8bc9",
        ),
        (
            "ghostty/vt/kitty_graphics.h",
            "6e2fd91d7b390f65a8aece3ed8104d53e28a07a91f1e254e05f9c1a535da0ae0",
        ),
        (
            "ghostty/vt/modes.h",
            "6b6d7bc3cca252b893bb5e82f5d4b8cc42756a9566f083a45f946f0dd6376f0c",
        ),
        (
            "ghostty/vt/mouse.h",
            "7b9cd231878d3959de9b7d2762b3531c1dcf858dd5ffe95125283ab96342ef3c",
        ),
        (
            "ghostty/vt/mouse/encoder.h",
            "bb9f89ebc914ea89c226177a796216bed45cc62c2a8b9eaae5b1140e1630b703",
        ),
        (
            "ghostty/vt/mouse/event.h",
            "8b2006dc2f82701db2998966733b87167f3664dbf8c44b1559722e7e03979216",
        ),
        (
            "ghostty/vt/osc.h",
            "b5aa3d38426f013b0c0c9935aa4d456956d89461043925d181a2770951b50f84",
        ),
        (
            "ghostty/vt/paste.h",
            "844127fef490443117b3bebc18bb0405f703c4379fa6c9f029d310807ae74db5",
        ),
        (
            "ghostty/vt/point.h",
            "c7e0803840253812a889424997225d47f4fa33c405804f151f38662daca9f332",
        ),
        (
            "ghostty/vt/render.h",
            "6a8776ab477083230041c41078d6c26e2050c209d14921557b8f96f6ec4c1bed",
        ),
        (
            "ghostty/vt/screen.h",
            "5b1c2f644d5648f5039112a532314be17c4db40b4de5e1af0c4b66d511a07261",
        ),
        (
            "ghostty/vt/search.h",
            "c6f77f31e2b3b6a5fef0fef0bd0026a8b69fb7dc438ea6e45930295634ec97d8",
        ),
        (
            "ghostty/vt/selection.h",
            "d8e8b718350a0d2f044dc9bb61479ea669627052c779d64a6cd52e1ed17126e8",
        ),
        (
            "ghostty/vt/sgr.h",
            "9c7f489a07360ec466a9888b487a1bd3527779427a96e0434ab40e42f607662e",
        ),
        (
            "ghostty/vt/size_report.h",
            "2f2670b38a35c46ec90533716afc8d7da8cf530583ca3d3126b20aba53409117",
        ),
        (
            "ghostty/vt/snapshot.h",
            "06458be017740d0a3637f090b07097179bdf97fc4b12f7fe5e21690712f97790",
        ),
        (
            "ghostty/vt/style.h",
            "6d7ddf28df06bfa2c8a60689928d02ebb0639b6161df9249cf224a0215a1f5c6",
        ),
        (
            "ghostty/vt/sys.h",
            "075dfb3f05c5a96bec1e37beada45ee5c73d08ee52f88228dceb7798ede6114c",
        ),
        (
            "ghostty/vt/terminal.h",
            "13dbd0b137a5503d9c842a67b4850e4a1d64d88ba28d77e1f31dc1e62679eae7",
        ),
        (
            "ghostty/vt/types.h",
            "0839958c8d4c967d6d8a0f89b963e4623b89e9c2206dae8ed0bbd9b3bc1c5415",
        ),
        (
            "ghostty/vt/unicode.h",
            "aafc1ce540d65976259e9c15c2fe4507809bcfbc9ffb48de7fcb1fbbbf892010",
        ),
        (
            "ghostty/vt/version.h",
            "9d750c7a61aa10faa70253179c330b0df46cf2a669c4ae41214500b6bb1429dc",
        ),
        (
            "ghostty/vt/wasm.h",
            "af256558795c9675dbec5b76af924c2b2e78c5aacede2fc0affdfedad1980096",
        ),
    ];
    println!("cargo:rerun-if-env-changed=TERMINAL_WORKSPACE_GHOSTTY_RELEASE_STAGE");
    println!("cargo:rerun-if-env-changed=TERMINAL_WORKSPACE_GHOSTTY_STAGE");
    println!("cargo:rerun-if-changed=native/ghostty_bridge.c");
    println!("cargo:rerun-if-changed=native/ghostty_bridge.h");
    assert_eq!(env::var("TARGET").unwrap(), "aarch64-apple-darwin");
    let stage = PathBuf::from(
        env::var("TERMINAL_WORKSPACE_GHOSTTY_RELEASE_STAGE")
            .expect("Official Ghostty stage required"),
    );
    assert_eq!(
        stage
            .canonicalize()
            .expect("Official Ghostty stage unavailable"),
        stage,
        "Stage symlinks rejected"
    );
    assert!(
        stage.parent() == Some(Path::new("/private/tmp"))
            && stage
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("twr-ghostty-release-1-")
    );
    let owner = fs::metadata(env::var("OUT_DIR").unwrap()).unwrap().uid();
    let stage_meta = fs::symlink_metadata(&stage).unwrap();
    assert!(
        stage_meta.is_dir()
            && stage_meta.uid() == owner
            && stage_meta.permissions().mode() & 0o077 == 0,
        "Official stage must be private and owned"
    );
    fn verify_file(path: &Path, owner: u32, cap: u64, expected: &str) {
        assert_eq!(
            path.canonicalize().unwrap(),
            path,
            "Symlink rejected: {}",
            path.display()
        );
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(
            meta.is_file() && meta.uid() == owner && meta.len() <= cap,
            "Invalid file: {}",
            path.display()
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(path).unwrap())),
            expected,
            "Digest changed: {}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
    }
    fn walk_headers(base: &Path, path: &Path, owner: u32, found: &mut BTreeSet<String>) {
        assert_eq!(
            path.canonicalize().unwrap(),
            path,
            "Header directory symlink rejected"
        );
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(
            meta.is_dir() && meta.uid() == owner,
            "Invalid header directory"
        );
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let child = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk_headers(base, &child, owner, found);
            } else {
                assert!(kind.is_file(), "Unexpected header entry");
                assert!(found.len() < 36, "Too many headers");
                assert!(found.insert(
                    child
                        .strip_prefix(base)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned()
                ));
            }
        }
    }
    let zip = stage.join("ghostty-vt-1.0.0.xcframework.zip");
    let library = stage.join("libghostty-vt.a");
    verify_file(&zip, owner, 32 * 1024 * 1024, ZIP_HASH);
    verify_file(&library, owner, 64 * 1024 * 1024, LIB_HASH);
    let headers = stage.join("Headers");
    let mut found = BTreeSet::new();
    walk_headers(&headers, &headers, owner, &mut found);
    let expected: BTreeSet<String> = HEADERS.iter().map(|(name, _)| (*name).to_owned()).collect();
    assert_eq!(found, expected, "Unexpected release header inventory");
    for (name, digest) in HEADERS {
        verify_file(&headers.join(name), owner, 256 * 1024, digest);
    }
    cc::Build::new()
        .compiler("/usr/bin/clang")
        .file("native/ghostty_bridge.c")
        .include(&headers)
        .define("GHOSTTY_STATIC", "1")
        .flag("-std=c11")
        .warnings_into_errors(true)
        .compile("terminal_ghostty_bridge");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let copied = out.join("libterminal_ghostty_core.a");
    fs::copy(&library, &copied).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&copied).unwrap())),
        LIB_HASH,
        "Copied official core changed"
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=terminal_ghostty_core");
    println!("cargo:rustc-env=TERMINAL_WORKSPACE_GHOSTTY_COMMIT={COMMIT}");
    println!("cargo:rustc-env=TERMINAL_WORKSPACE_GHOSTTY_ARCHIVE_SHA256={LIB_HASH}");
}
