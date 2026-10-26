// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Package loading seam: extract tar.gz and parse manifest.
//! Tests can use this with fixture tarballs or inject (extract_dir, manifest).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use baml_rt_core::{AgentManifest, BamlRtError, Result, join_error_message};
use baml_rt_observability::spans;
use tracing::info;

fn next_extract_dir() -> Result<PathBuf> {
    static EXTRACT_COUNTER: AtomicU64 = AtomicU64::new(0);
    let epoch_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
        .as_nanos();
    let pid = std::process::id();
    let seq = EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(std::env::temp_dir().join(format!("baml-agent-{epoch_nanos}-{pid}-{seq}")))
}

/// Load an agent package from a tar.gz path: extract to a temp dir and return
/// (extract_dir, validated manifest). Caller is responsible for ensuring
/// baml_src exists under extract_dir if they need it.
pub async fn load_package(package_path: &Path) -> Result<(PathBuf, AgentManifest)> {
    let package_path = package_path.to_path_buf();
    tokio::task::spawn_blocking(move || load_package_blocking(&package_path))
        .await
        .map_err(|e| blocking_join_error("agent package load", e))?
}

pub(crate) fn blocking_join_error(operation: &str, err: tokio::task::JoinError) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(join_error_message(operation, err)))
}

fn load_package_blocking(package_path: &Path) -> Result<(PathBuf, AgentManifest)> {
    let span = spans::load_agent_package(package_path);
    let _guard = span.enter();

    let extract_dir = next_extract_dir()?;
    std::fs::create_dir_all(&extract_dir).map_err(BamlRtError::Io)?;

    {
        let extract_span = spans::extract_package(&extract_dir);
        let _extract_guard = extract_span.enter();

        extract_validated_package(package_path, &extract_dir)?;
    }

    let manifest_path = extract_dir.join("manifest.json");
    let manifest_content = std::fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
    let manifest: AgentManifest =
        serde_json::from_str(&manifest_content).map_err(BamlRtError::Json)?;

    info!(
        name = manifest.name,
        version = manifest.version,
        entry_point = manifest.entry_point,
        "Agent manifest loaded"
    );

    let baml_src = extract_dir.join("baml_src");
    if !baml_src.exists() {
        return Err(BamlRtError::InvalidArgument(
            "Package missing baml_src directory".to_string(),
        ));
    }

    Ok((extract_dir, manifest))
}

/// Extract a package after validating every entry, before anything touches the destination.
///
/// Duplicate paths, case collisions, traversal components, absolute paths, symlinks, and
/// other non-regular entries are rejected. This is the same path/entry policy the identity
/// collector applies to an archive, so extraction and identity cannot disagree about what
/// the package contains.
fn extract_validated_package(package_path: &Path, extract_dir: &Path) -> Result<()> {
    let tar_gz = std::fs::File::open(package_path).map_err(BamlRtError::Io)?;
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut dirs: BTreeMap<String, String> = BTreeMap::new();

    for entry in archive.entries().map_err(BamlRtError::Io)? {
        let mut entry = entry.map_err(BamlRtError::Io)?;
        let entry_type = entry.header().entry_type();
        let raw_path = entry.path().map_err(BamlRtError::Io)?;
        let mut path = raw_path.to_string_lossy().replace('\\', "/");
        let is_dir = entry_type.is_dir();
        if is_dir {
            path = path.trim_end_matches('/').to_string();
        }
        baml_rt_repository::identity::validate_package_path(&path).map_err(|e| {
            BamlRtError::InvalidArgument(format!("invalid package entry path: {e}"))
        })?;

        register_package_path(&path, is_dir, &mut files, &mut dirs)?;

        if is_dir {
            std::fs::create_dir_all(extract_dir.join(&path)).map_err(BamlRtError::Io)?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(BamlRtError::InvalidArgument(format!(
                "package archive contains a non-regular entry at `{path}`"
            )));
        }

        let destination = extract_dir.join(&path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        let mut output = std::fs::File::create(&destination).map_err(BamlRtError::Io)?;
        std::io::copy(&mut entry, &mut output).map_err(BamlRtError::Io)?;
    }
    Ok(())
}

fn register_package_path(
    path: &str,
    is_dir: bool,
    files: &mut BTreeMap<String, String>,
    dirs: &mut BTreeMap<String, String>,
) -> Result<()> {
    let components: Vec<&str> = path.split('/').collect();
    let parent_count = if is_dir {
        components.len()
    } else {
        components.len().saturating_sub(1)
    };
    let mut prefix = String::new();
    for component in &components[..parent_count] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        register_dir_path(&prefix, dirs)?;
    }
    if is_dir {
        return Ok(());
    }
    register_unique_path(&path.to_ascii_lowercase(), path, files, "file")
}

fn register_dir_path(path: &str, dirs: &mut BTreeMap<String, String>) -> Result<()> {
    let key = path.to_ascii_lowercase();
    match dirs.get(&key) {
        Some(existing) if existing != path => Err(BamlRtError::InvalidArgument(format!(
            "package archive contains case-colliding directory paths `{existing}` and `{path}`"
        ))),
        Some(_) => Ok(()),
        None => {
            dirs.insert(key, path.to_string());
            Ok(())
        }
    }
}

fn register_unique_path(
    key: &str,
    original: &str,
    table: &mut BTreeMap<String, String>,
    kind: &str,
) -> Result<()> {
    match table.get(key) {
        Some(existing) if existing == original => Err(BamlRtError::InvalidArgument(format!(
            "package archive contains duplicate {kind} path `{original}`"
        ))),
        Some(existing) => Err(BamlRtError::InvalidArgument(format!(
            "package archive contains case-colliding {kind} paths `{existing}` and `{original}`"
        ))),
        None => {
            table.insert(key.to_string(), original.to_string());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{load_package, next_extract_dir};

    #[test]
    fn next_extract_dir_is_unique() {
        let a = next_extract_dir().expect("first path");
        let b = next_extract_dir().expect("second path");
        assert_ne!(a, b, "extract directories must be unique");
    }

    #[tokio::test]
    async fn package_without_a_signature_loads() {
        let temp = tempfile::tempdir().expect("temporary package directory");
        let package_path = temp.path().join("agent.tar.gz");
        let package_file = fs::File::create(&package_path).expect("create package");
        let encoder = flate2::write::GzEncoder::new(package_file, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);

        append_file(
            &mut archive,
            "manifest.json",
            br#"{
                "version": "1.0.0",
                "name": "unsigned-agent",
                "entry_point": "dist/index.js"
            }"#,
        );
        append_file(&mut archive, "baml_src/agent.baml", b"client Test {}\n");
        let encoder = archive.into_inner().expect("finish tar archive");
        encoder.finish().expect("finish gzip package");

        let (extract_dir, manifest) = load_package(&package_path).await.expect("load package");
        assert_eq!(manifest.name, "unsigned-agent");
        assert!(manifest.signature.is_empty());
        fs::remove_dir_all(extract_dir).expect("remove extracted package");
    }

    fn append_file<W: std::io::Write>(archive: &mut tar::Builder<W>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).expect("valid archive path");
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append(&header, bytes).expect("append package file");
    }

    fn package_bytes(
        append: impl FnOnce(&mut tar::Builder<&mut flate2::write::GzEncoder<Vec<u8>>>),
    ) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut archive = tar::Builder::new(&mut encoder);
            append(&mut archive);
            archive.finish().expect("finish tar");
        }
        encoder.finish().expect("finish gzip")
    }

    async fn load_from_bytes(bytes: Vec<u8>) -> baml_rt_core::Result<()> {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("agent.tar.gz");
        fs::write(&path, bytes).expect("write package");
        load_package(&path).await.map(|_| ())
    }

    #[tokio::test]
    async fn duplicate_archive_entries_are_rejected_before_extraction() {
        let bytes = package_bytes(|archive| {
            append_file(archive, "manifest.json", br#"{"name":"dup","version":"1"}"#);
            append_file(archive, "baml_src/a.baml", b"function A() -> string\n");
            append_file(archive, "baml_src/a.baml", b"function A() -> string\n");
        });
        let err = load_from_bytes(bytes).await.expect_err("duplicate entry");
        assert!(err.to_string().contains("duplicate file path"), "{err}");
    }

    #[tokio::test]
    async fn case_colliding_archive_entries_are_rejected() {
        let bytes = package_bytes(|archive| {
            append_file(
                archive,
                "manifest.json",
                br#"{"name":"case","version":"1"}"#,
            );
            append_file(archive, "baml_src/A.baml", b"function A() -> string\n");
            append_file(archive, "BAML_SRC/a.baml", b"function A() -> string\n");
        });
        let err = load_from_bytes(bytes).await.expect_err("case collision");
        assert!(
            err.to_string().contains("case-colliding directory"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn symlink_archive_entries_are_rejected() {
        let bytes = package_bytes(|archive| {
            append_file(
                archive,
                "manifest.json",
                br#"{"name":"link","version":"1"}"#,
            );
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_path("baml_src/link").expect("path");
            header.set_link_name("/etc/passwd").expect("link name");
            header.set_cksum();
            archive.append(&header, &[][..]).expect("append symlink");
        });
        let err = load_from_bytes(bytes).await.expect_err("symlink");
        assert!(err.to_string().contains("non-regular entry"), "{err}");
    }

    fn raw_archive_with_path(path: &str) -> Vec<u8> {
        let mut tar = vec![0_u8; 512];
        tar[..path.len()].copy_from_slice(path.as_bytes());
        tar[100..108].copy_from_slice(b"0000644\0");
        tar[124..136].copy_from_slice(b"00000000002\0");
        tar[148..156].fill(b' ');
        tar[156] = b'0';
        tar[257..263].copy_from_slice(b"ustar\0");
        let checksum: u32 = tar.iter().map(|byte| u32::from(*byte)).sum();
        let encoded = format!("{checksum:06o}\0 ");
        tar[148..156].copy_from_slice(encoded.as_bytes());
        tar.extend_from_slice(b"{}\n");
        tar.extend_from_slice(&[0; 1024]);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        gzip.write_all(&tar).expect("write raw archive");
        gzip.finish().expect("finish gzip")
    }

    #[tokio::test]
    async fn traversal_archive_entries_are_rejected() {
        let err = load_from_bytes(raw_archive_with_path("../manifest.json"))
            .await
            .expect_err("traversal");
        assert!(
            err.to_string().contains("invalid package entry path"),
            "{err}"
        );
    }
}
