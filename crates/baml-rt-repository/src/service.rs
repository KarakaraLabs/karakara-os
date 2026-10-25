// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Repository service — orchestrates storage, hashing, and lineage.
//!
//! This is the primary API surface for the repository. It owns the stores and
//! coordinates multi-step operations (publish, fork, search) that span blob
//! storage, metadata, and lineage.

use std::sync::Arc;

use baml_rt_core::clock_events;
use baml_rt_tools::{
    StaticToolCatalogResponse, external_tools::ExternalToolSnapshot,
    mcp_snapshot::McpServerSnapshot,
};

use crate::{
    commands::{ForkCommand, PublishCommand, PublishOrigin, PublishResult},
    entry::{NewEntry, RepositoryEntry, RepositoryEntryHeader, Tag, Timestamp, VersionReservation},
    error::{RepositoryError, Result},
    external_tool::{ExternalToolRegistryTool, ExternalToolRegistryToolVersion},
    ids::{AgentName, ArtifactHash, ContentHash, Generation, LineageEdgeId, Version},
    lineage::{EdgeDescription, LineageEdge, LineageKind, LineageSubgraph, Parentage},
    mcp::{McpRegistryServer, McpRegistryServerVersion, McpRegistryToolVersion},
    search::SearchQuery,
    storage::{
        BlobStore, ExternalToolRegistryStore, LineageStore, McpRegistryStore, MetadataStore,
        SearchStore,
    },
};

/// Default lifetime of a version reservation held by one publisher.
///
/// A publisher reserves before building, so this must comfortably exceed a full build. A
/// crashed publisher blocks its version for at most this long. A gap left behind costs
/// nothing: versions are monotonic labels, not dense counters.
pub const DEFAULT_VERSION_RESERVATION_TTL_MS: u64 = 600_000;

/// The main repository service.
///
/// Coordinates blob storage, metadata, lineage, and search stores to
/// implement publish, fork, search, and retrieval operations.
pub struct RepositoryService {
    blobs: Arc<dyn BlobStore>,
    metadata: Arc<dyn MetadataStore>,
    lineage: Arc<dyn LineageStore>,
    search: Arc<dyn SearchStore>,
    mcp_registry: Arc<dyn McpRegistryStore>,
    external_tool_registry: Arc<dyn ExternalToolRegistryStore>,
    /// Static (compiled-in) tool catalog of the host process, if injected.
    ///
    /// The repository service owns no tool inventory; the host runner projects
    /// its own linked `inventory` into a [`StaticToolCatalogResponse`] at
    /// startup and injects it here so the read API can serve the slim-runner
    /// reality. `None` for detached repository services (no inventory to serve).
    static_tool_catalog: Option<StaticToolCatalogResponse>,
}

impl RepositoryService {
    /// Create a new service with the given store implementations.
    pub fn new(
        blobs: Arc<dyn BlobStore>,
        metadata: Arc<dyn MetadataStore>,
        lineage: Arc<dyn LineageStore>,
        search: Arc<dyn SearchStore>,
        mcp_registry: Arc<dyn McpRegistryStore>,
        external_tool_registry: Arc<dyn ExternalToolRegistryStore>,
    ) -> Self {
        Self {
            blobs,
            metadata,
            lineage,
            search,
            mcp_registry,
            external_tool_registry,
            static_tool_catalog: None,
        }
    }

    /// Inject the host runner's static tool catalog so the read API can serve
    /// `GET /repository/static-tools/snapshots`. Builder-style; call once at
    /// startup after constructing the service from stores.
    pub fn with_static_tool_catalog(mut self, catalog: StaticToolCatalogResponse) -> Self {
        self.static_tool_catalog = Some(catalog);
        self
    }

    /// The injected static tool catalog, if the host runner provided one.
    pub fn static_tool_catalog(&self) -> Option<&StaticToolCatalogResponse> {
        self.static_tool_catalog.as_ref()
    }

    // -----------------------------------------------------------------------
    // MCP registry
    // -----------------------------------------------------------------------

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpRegistryServer>> {
        self.mcp_registry.list_mcp_servers().await
    }

    pub async fn put_mcp_snapshot(
        &self,
        snapshot: &McpServerSnapshot,
    ) -> Result<McpRegistryServerVersion> {
        self.mcp_registry.put_mcp_snapshot(snapshot).await
    }

    pub async fn get_mcp_snapshot(
        &self,
        server_id: &str,
        version: u32,
    ) -> Result<Option<McpServerSnapshot>> {
        self.mcp_registry.get_mcp_snapshot(server_id, version).await
    }

    pub async fn get_latest_mcp_snapshot(
        &self,
        server_id: &str,
    ) -> Result<Option<McpServerSnapshot>> {
        self.mcp_registry.get_latest_mcp_snapshot(server_id).await
    }

    pub async fn list_mcp_server_versions(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpRegistryServerVersion>> {
        self.mcp_registry.list_mcp_server_versions(server_id).await
    }

    pub async fn find_mcp_tool(
        &self,
        platform_tool_name: &str,
    ) -> Result<Vec<McpRegistryToolVersion>> {
        self.mcp_registry.find_mcp_tool(platform_tool_name).await
    }

    pub async fn mark_mcp_version_stale(&self, server_id: &str, version: u32) -> Result<()> {
        self.mcp_registry
            .mark_mcp_version_stale(server_id, version)
            .await
    }

    // -----------------------------------------------------------------------
    // External-tool registry
    // -----------------------------------------------------------------------

    pub async fn list_external_tools(&self) -> Result<Vec<ExternalToolRegistryTool>> {
        self.external_tool_registry.list_external_tools().await
    }

    pub async fn put_external_tool_snapshot(
        &self,
        snapshot: &ExternalToolSnapshot,
    ) -> Result<ExternalToolRegistryToolVersion> {
        self.external_tool_registry
            .put_external_tool_snapshot(snapshot)
            .await
    }

    pub async fn get_external_tool_snapshot(
        &self,
        tool_name: &str,
        version: u32,
    ) -> Result<Option<ExternalToolSnapshot>> {
        self.external_tool_registry
            .get_external_tool_snapshot(tool_name, version)
            .await
    }

    pub async fn get_latest_external_tool_snapshot(
        &self,
        tool_name: &str,
    ) -> Result<Option<ExternalToolSnapshot>> {
        self.external_tool_registry
            .get_latest_external_tool_snapshot(tool_name)
            .await
    }

    pub async fn list_approved_external_tool_snapshots(&self) -> Result<Vec<ExternalToolSnapshot>> {
        self.external_tool_registry
            .list_approved_external_tool_snapshots()
            .await
    }

    pub async fn list_external_tool_versions(
        &self,
        tool_name: &str,
    ) -> Result<Vec<ExternalToolRegistryToolVersion>> {
        self.external_tool_registry
            .list_external_tool_versions(tool_name)
            .await
    }

    pub async fn mark_external_tool_version_stale(
        &self,
        tool_name: &str,
        version: u32,
    ) -> Result<()> {
        self.external_tool_registry
            .mark_external_tool_version_stale(tool_name, version)
            .await
    }

    // -----------------------------------------------------------------------
    // Publish
    // -----------------------------------------------------------------------

    /// Publish a new agent version.
    ///
    /// Reserves the version, then records parentage, inserts the entry at the reserved
    /// version, and writes lineage edges. The reservation is released on failure.
    pub async fn publish(&self, cmd: PublishCommand) -> Result<PublishResult> {
        let reservation = self.reserve_version(&cmd.name).await?;
        self.publish_reserved(cmd, &reservation, None).await
    }

    /// Reserve the next free version for `name`.
    ///
    /// Callers that build an artifact before publishing reserve first so the artifact's
    /// manifest and the stored entry carry the same version.
    pub async fn reserve_version(&self, name: &AgentName) -> Result<VersionReservation> {
        self.metadata
            .reserve_version(name, DEFAULT_VERSION_RESERVATION_TTL_MS)
            .await
    }

    /// Release a reservation that will not produce an entry.
    pub async fn release_version(&self, reservation: &VersionReservation) -> Result<()> {
        self.metadata.release_version(reservation).await
    }

    /// Publish a source bundle that realizes an already-built package artifact.
    ///
    /// The artifact must already be stored under `artifact_hash`. Prefer this over
    /// [`publish`](Self::publish) after a build: the caller holds the version reservation
    /// so the artifact manifest and the entry agree.
    pub async fn publish_built(
        &self,
        cmd: PublishCommand,
        reservation: &VersionReservation,
        artifact_hash: ArtifactHash,
    ) -> Result<PublishResult> {
        self.publish_reserved(cmd, reservation, Some(artifact_hash))
            .await
    }

    /// Publish at an explicitly reserved version.
    ///
    /// The reservation is released on failure. On success the store consumes it together
    /// with the entry insert.
    pub async fn publish_reserved(
        &self,
        cmd: PublishCommand,
        reservation: &VersionReservation,
        artifact_hash: Option<ArtifactHash>,
    ) -> Result<PublishResult> {
        if reservation.name != cmd.name {
            let _ = self.release_version(reservation).await;
            return Err(RepositoryError::VersionConflict {
                name: cmd.name.clone(),
                version: reservation.version,
            });
        }
        let result = self.finish_publish(cmd, reservation, artifact_hash).await;
        if result.is_err()
            && let Err(e) = self.release_version(reservation).await
        {
            tracing::warn!(
                agent = %reservation.name,
                version = reservation.version.as_u32(),
                error = %e,
                "failed to release version reservation after publish failure; it will expire"
            );
        }
        result
    }

    async fn finish_publish(
        &self,
        cmd: PublishCommand,
        reservation: &VersionReservation,
        artifact_hash: Option<ArtifactHash>,
    ) -> Result<PublishResult> {
        let _span = crate::spans::publish(cmd.name.as_str());

        // Determine parentage, generation, and deferred edge descriptors.
        // Edge targets will be filled with stored.hash after insert.
        let (parentage, generation, edge_descriptors) = match cmd.origin {
            PublishOrigin::Original => (Parentage::Original, Generation::ROOT, vec![]),

            PublishOrigin::Iteration => {
                match self.metadata.get_latest(&cmd.name).await? {
                    Some(prev) => {
                        let desc = EdgeDescription::new(format!(
                            "Iteration from {ver}",
                            ver = prev.version_ref
                        ))
                        .expect("non-empty");
                        (
                            Parentage::Forked {
                                parent: prev.hash.clone(),
                                description: desc.clone(),
                            },
                            prev.generation,
                            // (source_hash, kind, description) — target filled later
                            vec![(prev.hash, LineageKind::Fork, desc)],
                        )
                    }
                    None => (Parentage::Original, Generation::ROOT, vec![]),
                }
            }

            PublishOrigin::Influenced { influences } => {
                let mut descs = Vec::new();
                let mut max_gen = Generation::ROOT;

                for inf in &influences {
                    let source_entry = self.metadata.get_by_hash(&inf.source).await?;
                    match source_entry {
                        Some(e) => {
                            if e.generation > max_gen {
                                max_gen = e.generation;
                            }
                        }
                        None => {
                            return Err(RepositoryError::InfluenceSourceNotFound {
                                source_hash: inf.source.clone(),
                            });
                        }
                    }
                    descs.push((
                        inf.source.clone(),
                        LineageKind::Influence,
                        inf.description.clone(),
                    ));
                }

                (
                    Parentage::Synthesized {
                        influences: influences.clone(),
                    },
                    max_gen.increment(),
                    descs,
                )
            }
        };

        let source = cmd.source;
        let tags = manifest_tags(&source);
        let new_entry = NewEntry {
            name: cmd.name.clone(),
            artifact_hash,
            source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags,
        };

        // Insert at the reserved version. The store fails if the lineage no longer
        // agrees with it, so a concurrent publisher cannot steal this version.
        let stored = self
            .metadata
            .insert_reserved_entry(&new_entry, reservation)
            .await?;

        // Now create lineage edges using the store-assigned hash as target.
        if !edge_descriptors.is_empty() {
            let edges: Vec<LineageEdge> = edge_descriptors
                .into_iter()
                .map(|(source, kind, description)| LineageEdge {
                    id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                    source,
                    target: stored.hash.clone(),
                    kind,
                    description,
                })
                .collect();
            self.lineage.record_edges(&edges).await?;
        }

        tracing::info!(
            hash = %stored.hash,
            version = %stored.version_ref,
            generation = stored.generation.as_u32(),
            event = "published"
        );

        Ok(PublishResult {
            hash: stored.hash,
            artifact_hash: stored.artifact_hash,
            version_ref: stored.version_ref,
            generation: stored.generation,
        })
    }

    // -----------------------------------------------------------------------
    // Fork
    // -----------------------------------------------------------------------

    /// Fork an existing entry into a new lineage.
    pub async fn fork(&self, cmd: ForkCommand) -> Result<PublishResult> {
        let _span = crate::spans::fork(cmd.source_hash.as_str(), cmd.new_name.as_str());

        // Verify source exists
        let source_entry = self
            .metadata
            .get_by_hash(&cmd.source_hash)
            .await?
            .ok_or_else(|| RepositoryError::ForkParentNotFound {
                parent_hash: cmd.source_hash.clone(),
            })?;

        let generation = source_entry.generation.increment();

        let parentage = Parentage::Forked {
            parent: cmd.source_hash.clone(),
            description: cmd.fork_description.clone(),
        };

        let new_entry = NewEntry {
            name: cmd.new_name.clone(),
            artifact_hash: None,
            source: cmd.source,
            parentage,
            generation,
            change_rationale: cmd.rationale,
            tags: cmd.tags,
        };

        // Store atomically assigns version (v1 for a new lineage), writes it
        // into manifest, computes hash.
        let stored = self.metadata.insert_entry(&new_entry).await?;

        // Record fork edge using the store-assigned hash.
        let edge = LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: cmd.source_hash.clone(),
            target: stored.hash.clone(),
            kind: LineageKind::Fork,
            description: cmd.fork_description,
        };
        self.lineage.record_edges(&[edge]).await?;

        tracing::info!(
            hash = %stored.hash,
            version = %stored.version_ref,
            forked_from = %cmd.source_hash,
            event = "forked"
        );

        Ok(PublishResult {
            hash: stored.hash,
            artifact_hash: stored.artifact_hash,
            version_ref: stored.version_ref,
            generation: stored.generation,
        })
    }

    // -----------------------------------------------------------------------
    // Retrieval
    // -----------------------------------------------------------------------

    /// Get a full entry by content hash.
    pub async fn get_by_hash(&self, hash: &ContentHash) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_by_hash(hash).await
    }

    /// Get a full entry by name + version.
    pub async fn get_by_version(
        &self,
        name: &AgentName,
        version: Version,
    ) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_by_version(name, version).await
    }

    /// Get the latest version of an agent.
    pub async fn get_latest(&self, name: &AgentName) -> Result<Option<RepositoryEntry>> {
        self.metadata.get_latest(name).await
    }

    /// List all versions of an agent.
    pub async fn list_versions(&self, name: &AgentName) -> Result<Vec<RepositoryEntryHeader>> {
        self.metadata.list_versions(name).await
    }

    /// List all known agent names.
    pub async fn list_agents(&self) -> Result<Vec<AgentName>> {
        self.metadata.list_agents().await
    }

    /// Version number the next [`publish`](Self::publish) for this agent will receive.
    ///
    /// Counts live reservations as well as stored entries.
    pub async fn next_version_for_agent(&self, name: &AgentName) -> Result<u32> {
        Ok(self.metadata.next_version(name).await?.as_u32())
    }

    // -----------------------------------------------------------------------
    // Blob operations
    // -----------------------------------------------------------------------

    /// Retrieve a tar.gz package by its exact artifact hash and verify it before returning it.
    pub async fn get_blob(&self, hash: &ArtifactHash) -> Result<Option<Vec<u8>>> {
        let Some(bytes) = self.blobs.get_artifact(hash).await? else {
            return Ok(None);
        };
        let computed = ArtifactHash::from_bytes(&bytes);
        if &computed != hash {
            return Err(RepositoryError::ArtifactHashMismatch {
                expected: hash.clone(),
                computed,
            });
        }
        Ok(Some(bytes))
    }

    /// Store a repository-built tar.gz blob under its exact byte hash.
    ///
    /// This is intended for trusted in-process orchestrators (not public HTTP upload).
    pub async fn put_built_blob(&self, hash: &ArtifactHash, data: &[u8]) -> Result<()> {
        let computed = ArtifactHash::from_bytes(data);
        if &computed != hash {
            return Err(RepositoryError::ArtifactHashMismatch {
                expected: hash.clone(),
                computed,
            });
        }
        self.blobs.put_artifact(hash, data).await
    }

    /// Persist server-generated dev artifacts for a published package hash.
    pub async fn put_dev_artifacts(
        &self,
        package_hash: &ContentHash,
        bundle: &crate::dev_artifacts::DevArtifactsBundle,
    ) -> Result<()> {
        let blob_hash = crate::dev_artifacts::dev_artifacts_blob_hash(package_hash);
        let bytes = serde_json::to_vec(bundle).map_err(|e| RepositoryError::StorageWrite {
            source: Box::new(e),
        })?;
        self.blobs.put(&blob_hash, &bytes).await
    }

    /// Load dev artifacts for a published package hash.
    pub async fn get_dev_artifacts(
        &self,
        package_hash: &ContentHash,
    ) -> Result<Option<crate::dev_artifacts::DevArtifactsBundle>> {
        let blob_hash = crate::dev_artifacts::dev_artifacts_blob_hash(package_hash);
        let Some(bytes) = self.blobs.get(&blob_hash).await? else {
            return Ok(None);
        };
        let bundle = serde_json::from_slice(&bytes).map_err(|e| RepositoryError::StorageRead {
            source: Box::new(e),
        })?;
        Ok(Some(bundle))
    }

    // -----------------------------------------------------------------------
    // Lineage
    // -----------------------------------------------------------------------

    /// Get the lineage subgraph centered on an entry.
    pub async fn lineage(
        &self,
        hash: &ContentHash,
        ancestor_depth: u32,
    ) -> Result<LineageSubgraph> {
        self.lineage.subgraph(hash, ancestor_depth).await
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Execute a structured search query.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<RepositoryEntryHeader>> {
        self.search.search(query).await
    }

    // -----------------------------------------------------------------------
    // Metadata mutation
    // -----------------------------------------------------------------------

    /// Add a tag to an entry.
    pub async fn add_tag(&self, hash: &ContentHash, tag: Tag) -> Result<()> {
        self.metadata.add_tag(hash, tag).await
    }

    /// Remove a tag from an entry.
    pub async fn remove_tag(&self, hash: &ContentHash, tag: &Tag) -> Result<()> {
        self.metadata.remove_tag(hash, tag).await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// UTC timestamp in RFC 3339 format.
pub(crate) fn chrono_now() -> Timestamp {
    Timestamp::new(baml_rt_core::now_unix_secs(clock_events::REPOSITORY_TIMESTAMP).to_string())
}

fn manifest_tags(source: &crate::entry::SourceBundle) -> Vec<Tag> {
    source
        .manifest
        .tags()
        .into_iter()
        .map(|tag| Tag::new(tag.to_string()))
        .collect()
}
