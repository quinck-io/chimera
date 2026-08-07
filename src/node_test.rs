use super::*;

fn runtimes(entries: &[(&str, &str)]) -> NodeRuntimes {
    let by_major: BTreeMap<String, PathBuf> = entries
        .iter()
        .map(|(major, path)| ((*major).to_string(), PathBuf::from(path)))
        .collect();
    let default_path = by_major
        .get(DEFAULT_MAJOR)
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/externals/node20-linux-x64/bin/node"));
    NodeRuntimes {
        by_major,
        default_path,
    }
}

fn both() -> NodeRuntimes {
    runtimes(&[
        ("20", "/externals/node20-linux-x64/bin/node"),
        ("24", "/externals/node24-linux-x64/bin/node"),
    ])
}

#[test]
fn resolves_the_declared_major() {
    let rt = both();

    assert_eq!(
        rt.resolve(Some("24")),
        Path::new("/externals/node24-linux-x64/bin/node")
    );
    assert_eq!(
        rt.resolve(Some("20")),
        Path::new("/externals/node20-linux-x64/bin/node")
    );
}

#[test]
fn falls_back_when_the_action_declares_nothing() {
    assert_eq!(
        both().resolve(None),
        Path::new("/externals/node20-linux-x64/bin/node")
    );
}

/// node12 and node16 actions still exist in the wild and must not hard-fail.
#[test]
fn falls_back_for_a_runtime_we_do_not_ship() {
    assert_eq!(
        both().resolve(Some("16")),
        Path::new("/externals/node20-linux-x64/bin/node")
    );
}

#[test]
fn falls_back_when_a_download_was_skipped() {
    let rt = runtimes(&[("20", "/externals/node20-linux-x64/bin/node")]);

    assert_eq!(
        rt.resolve(Some("24")),
        Path::new("/externals/node20-linux-x64/bin/node")
    );
}

#[test]
fn remaps_every_runtime_under_the_container_mount() {
    let remapped = both().remapped("/github/externals");

    assert_eq!(
        remapped.get("20").map(String::as_str),
        Some("/github/externals/node20-linux-x64/bin/node")
    );
    assert_eq!(
        remapped.get("24").map(String::as_str),
        Some("/github/externals/node24-linux-x64/bin/node")
    );
}

#[test]
fn every_shipped_version_matches_its_major() {
    for (major, version) in NODE_VERSIONS {
        assert!(
            version.starts_with(&format!("v{major}.")),
            "{version} is not a Node {major} release"
        );
    }
    assert!(
        NODE_VERSIONS
            .iter()
            .any(|(major, _)| *major == DEFAULT_MAJOR),
        "the fallback major must be one we ship"
    );
}
