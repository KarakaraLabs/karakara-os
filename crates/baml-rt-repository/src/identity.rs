// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Select the behavior-defining files from a *packaged* agent representation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read},
    path::Path,
};

use baml_rt_core::AgentManifest;
use baml_rt_hash::{
    AgentIdentityDigest, AgentIdentityEntry, AgentIdentityError, compute_agent_identity_digest,
};
use flate2::read::GzDecoder;
use serde_json::{Map, Value};
use tar::Archive;
use thiserror::Error;

const MANIFEST: &str = "manifest.json";
const SESSION_PLAN_FUNCTIONS: &str = "session_plan_functions.json";
const TOOL_SCHEMA_CATALOG: &str = "baml_src/_baml_tool_schema_catalog.txt";

#[derive(Debug, Error)]
pub enum PackageIdentityError {
    #[error("failed to read package archive: {0}")]
    Archive(String),
    #[error("invalid package archive path `{path}`")]
    InvalidPath { path: String },
    #[error("package archive contains a non-regular entry at `{path}`")]
    NonRegularEntry { path: String },
    #[error("package contains duplicate or case-colliding path `{path}`")]
    DuplicatePath { path: String },
    #[error("package manifest.json is missing")]
    ManifestMissing,
    #[error("failed to parse package manifest: {0}")]
    ManifestParse(String),
    #[error("package baml_src directory is missing")]
    BamlSourceMissing,
    #[error("package entry point `{entry_point}` is missing or is not a file")]
    EntryPointMissing { entry_point: String },
    #[error("failed to read package file `{path}`: {reason}")]
    FileRead { path: String, reason: String },
    #[error("identity entry error: {0}")]
    Entry(#[from] AgentIdentityError),
}

/// Collect the canonical behavior entries from a packaged `.tar.gz` artifact.
pub fn identity_entries_from_tar_gz(
    bytes: &[u8],
) -> Result<Vec<AgentIdentityEntry>, PackageIdentityError> {
    let mut archive = Archive::new(GzDecoder::new(Cursor::new(bytes)));
    let mut files = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut has_baml_dir = false;
    for entry in archive
        .entries()
        .map_err(|e| PackageIdentityError::Archive(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| PackageIdentityError::Archive(e.to_string()))?;
        let entry_type = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|e| PackageIdentityError::Archive(e.to_string()))?;
        let mut path = path.to_string_lossy().replace('\\', "/");
        if entry_type.is_dir() {
            path = path.trim_end_matches('/').to_owned();
        }
        validate_package_path(&path)?;
        if !paths.insert(path.to_ascii_lowercase()) {
            return Err(PackageIdentityError::DuplicatePath { path });
        }
        if path == "baml_src" {
            has_baml_dir = true;
        }
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(PackageIdentityError::NonRegularEntry { path });
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| PackageIdentityError::FileRead {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        if path.starts_with("baml_src/") {
            has_baml_dir = true;
        }
        files.insert(path, content);
    }
    entries_from_files(files, has_baml_dir)
}

/// Compute the identity digest for a packaged `.tar.gz` artifact.
///
/// This is the value the repository stores on the published entry. It is derived from the exact
/// packaged bytes, so it agrees with the digest a runner recomputes from the extracted package
/// at boot.
pub fn identity_digest_from_tar_gz(
    bytes: &[u8],
) -> Result<AgentIdentityDigest, PackageIdentityError> {
    let entries = identity_entries_from_tar_gz(bytes)?;
    compute_agent_identity_digest(&entries).map_err(PackageIdentityError::Entry)
}

/// Collect the canonical behavior entries from a freshly extracted package directory.
///
/// This deliberately accepts an extracted package only, not an authored source directory.
pub fn identity_entries_from_extracted_package(
    root: &Path,
) -> Result<Vec<AgentIdentityEntry>, PackageIdentityError> {
    let metadata = fs::metadata(root).map_err(|e| PackageIdentityError::FileRead {
        path: root.display().to_string(),
        reason: e.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(PackageIdentityError::InvalidPath {
            path: root.display().to_string(),
        });
    }
    let mut files = BTreeMap::new();
    collect_regular_files(root, root, &mut files)?;
    entries_from_files(files, root.join("baml_src").is_dir())
}

fn collect_regular_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), PackageIdentityError> {
    for entry in fs::read_dir(current).map_err(|e| PackageIdentityError::FileRead {
        path: current.display().to_string(),
        reason: e.to_string(),
    })? {
        let entry = entry.map_err(|e| PackageIdentityError::FileRead {
            path: current.display().to_string(),
            reason: e.to_string(),
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|e| PackageIdentityError::FileRead {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(PackageIdentityError::NonRegularEntry {
                path: path.display().to_string(),
            });
        }
        if metadata.is_dir() {
            collect_regular_files(root, &path, files)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("walked below root")
            .to_string_lossy()
            .replace('\\', "/");
        validate_package_path(&relative)?;
        let bytes = fs::read(&path).map_err(|e| PackageIdentityError::FileRead {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        if files.insert(relative.clone(), bytes).is_some() {
            return Err(PackageIdentityError::DuplicatePath { path: relative });
        }
    }
    Ok(())
}

fn entries_from_files(
    mut files: BTreeMap<String, Vec<u8>>,
    has_baml_dir: bool,
) -> Result<Vec<AgentIdentityEntry>, PackageIdentityError> {
    let mut paths = BTreeSet::new();
    for path in files.keys() {
        if !paths.insert(path.to_ascii_lowercase()) {
            return Err(PackageIdentityError::DuplicatePath { path: path.clone() });
        }
    }
    if !has_baml_dir {
        return Err(PackageIdentityError::BamlSourceMissing);
    }
    let manifest_bytes = files
        .remove(MANIFEST)
        .ok_or(PackageIdentityError::ManifestMissing)?;
    let manifest: AgentManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| PackageIdentityError::ManifestParse(e.to_string()))?;
    if !files.contains_key(&manifest.entry_point) {
        return Err(PackageIdentityError::EntryPointMissing {
            entry_point: manifest.entry_point,
        });
    }

    let mut entries = vec![AgentIdentityEntry::from_bytes(
        MANIFEST,
        &manifest_projection(&manifest),
    )?];
    for (path, bytes) in files {
        if is_behavior_path(&path) {
            entries.push(AgentIdentityEntry::from_bytes(path, &bytes)?);
        }
    }
    Ok(entries)
}

fn is_behavior_path(path: &str) -> bool {
    (path.starts_with("src/") && path.ends_with(".ts") && !path.ends_with(".d.ts"))
        || (path.starts_with("baml_src/") && path.ends_with(".baml"))
        || path == TOOL_SCHEMA_CATALOG
        || path == SESSION_PLAN_FUNCTIONS
}

fn manifest_projection(manifest: &AgentManifest) -> Vec<u8> {
    // BTreeMap fixes key ordering independently of serde_json's map feature.
    let mut projection = BTreeMap::new();
    projection.insert(
        "discovery",
        manifest
            .discovery
            .as_ref()
            .map(canonical_value)
            .unwrap_or(Value::Null),
    );
    projection.insert("entry_point", Value::String(manifest.entry_point.clone()));
    projection.insert("name", Value::String(manifest.name.clone()));
    projection.insert("tags", canonical_value(&manifest.tags));
    projection.insert("tools", canonical_value(&manifest.tools));
    serde_json::to_vec(&projection).expect("manifest projection is serializable")
}

fn canonical_value<T: serde::Serialize>(value: &T) -> Value {
    sort_json(serde_json::to_value(value).expect("manifest value is serializable"))
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        scalar => scalar,
    }
}

fn validate_package_path(path: &str) -> Result<(), PackageIdentityError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains("\\")
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(PackageIdentityError::InvalidPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use baml_rt_core::ids::AgentId;
    use baml_rt_hash::compute_agent_identity_digest;
    use flate2::{Compression, write::GzEncoder};
    use tar::{Builder, Header};
    use tempfile::tempdir;

    use super::{
        PackageIdentityError, identity_entries_from_extracted_package, identity_entries_from_tar_gz,
    };

    fn package(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut tar = Builder::new(&mut gzip);
            for (path, content) in entries {
                let mut header = Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, path, *content).unwrap();
            }
            tar.finish().unwrap();
        }
        gzip.finish().unwrap()
    }

    fn raw_package(path: &str, entry_type: u8) -> Vec<u8> {
        let mut tar = vec![0_u8; 512];
        tar[..path.len()].copy_from_slice(path.as_bytes());
        tar[100..108].copy_from_slice(b"0000644\0");
        tar[124..136].copy_from_slice(b"00000000000\0");
        tar[148..156].fill(b' ');
        tar[156] = entry_type;
        tar[257..263].copy_from_slice(b"ustar\0");
        let checksum: u32 = tar.iter().map(|byte| u32::from(*byte)).sum();
        let encoded = format!("{checksum:06o}\0 ");
        tar[148..156].copy_from_slice(encoded.as_bytes());
        tar.extend_from_slice(&[0; 1024]);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    fn fixture_entries() -> Vec<(&'static str, &'static [u8])> {
        vec![
            (
                "manifest.json",
                br#"{"version":"9","signature":"ignored","name":"golden-agent","tools":["B","A"]}"#,
            ),
            ("src/index.ts", b"export const prompt = 'hello';\n"),
            ("src/baml-runtime.d.ts", b"excluded"),
            ("baml_src/main.baml", b"function Run() -> string\n"),
            ("baml_src/_baml_runtime.baml", b"generated prompt\n"),
            ("baml_src/_baml_tool_schema_catalog.txt", b"tool schema\n"),
            ("session_plan_functions.json", b"{\"Run\":\"run\"}\n"),
            ("dist/index.js", b"excluded build output"),
        ]
    }

    #[test]
    fn identity_golden_vector() {
        let digest = compute_agent_identity_digest(
            &identity_entries_from_tar_gz(&package(&fixture_entries())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            digest.as_str(),
            "a01024aaacdbfe2e867698f25f56a4ec6dcb3362af0d6264c6ab2b688eba9cb8"
        );
        assert_eq!(
            AgentId::from_identity_digest(&digest).as_str(),
            "3893af74-6592-537d-a1dc-1fea8c6721c3"
        );
    }

    #[test]
    fn collection_paths_and_archive_hygiene() {
        let bytes = package(&fixture_entries());
        let archive_entries = identity_entries_from_tar_gz(&bytes).unwrap();
        let root = tempdir().unwrap();
        for (path, content) in fixture_entries() {
            let target = root.path().join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::File::create(target)
                .unwrap()
                .write_all(content)
                .unwrap();
        }
        let extracted_entries = identity_entries_from_extracted_package(root.path()).unwrap();
        assert_eq!(archive_entries, extracted_entries);

        for path in ["../manifest.json", "/manifest.json"] {
            assert!(matches!(
                identity_entries_from_tar_gz(&raw_package(path, b'0')),
                Err(PackageIdentityError::InvalidPath { .. })
            ));
        }
        assert!(matches!(
            identity_entries_from_tar_gz(&raw_package("linked", b'2')),
            Err(PackageIdentityError::NonRegularEntry { .. })
        ));
        assert!(matches!(
            identity_entries_from_tar_gz(&package(&[
                ("manifest.json", b"{}"),
                ("manifest.json", b"{}")
            ])),
            Err(PackageIdentityError::DuplicatePath { .. })
        ));
    }
}
