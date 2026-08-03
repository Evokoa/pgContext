//! Reproducible large-scale certification fixture manifests and generators.

use std::io::{self, Read, Write};

const GENERATOR_VERSION: u16 = 1;
const CHECKSUM_ALGORITHM: &str = "fnv1a64-v1";
const EXACT_QUERY_COUNT: u32 = 1_024;
const EXACT_K: usize = 10;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const EXACT_DOMAIN: u64 = 0x6578_6163_745f_7631;
const FILTER_DOMAIN: u64 = 0x6669_6c74_6572_5f31;
const VECTOR_DOMAIN: u64 = 0x7665_6374_6f72_5f31;

/// One ordered exact nearest-neighbor result retained in certification evidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExactNeighbor {
    point_id: u64,
    squared_l2: f64,
}

impl ExactNeighbor {
    /// Returns the authoritative point identifier.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Returns the exact squared-L2 score used for ordering.
    #[must_use]
    pub const fn squared_l2(self) -> f64 {
        self.squared_l2
    }
}

/// Required certification workload tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificationTier {
    /// One hundred thousand authoritative rows.
    Rows100k,
    /// One million authoritative rows.
    Rows1m,
    /// Ten million authoritative rows.
    Rows10m,
}

/// Retained fixture artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificationArtifactKind {
    /// Compact authoritative row records from which vectors are expanded.
    RawInputs,
    /// Exact filtered nearest-point oracle rows.
    ExactGroundTruth,
}

/// One retained, checksummed certification artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificationArtifactManifest {
    kind: CertificationArtifactKind,
    relative_path: &'static str,
    checksum: u64,
}

impl CertificationArtifactManifest {
    /// Returns the artifact role.
    #[must_use]
    pub const fn kind(self) -> CertificationArtifactKind {
        self.kind
    }

    /// Returns the repository-relative retention path.
    #[must_use]
    pub const fn relative_path(self) -> &'static str {
        self.relative_path
    }

    /// Returns the complete artifact checksum.
    #[must_use]
    pub const fn checksum(self) -> u64 {
        self.checksum
    }

    /// Returns the versioned checksum algorithm.
    #[must_use]
    pub const fn checksum_algorithm(self) -> &'static str {
        CHECKSUM_ALGORITHM
    }
}

/// Reproducible dataset and exact-ground-truth contract.
///
/// `generate_artifact` is the canonical streaming encoder for both retained
/// files. Raw rows contain stable point, tenant, filter, and vector-seed fields;
/// vector components are counter-generated from that seed and dimension. Exact
/// rows retain the controlled-filter point oracle for fixed query seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificationDatasetManifest {
    tier: CertificationTier,
    rows: u64,
    dimensions: u16,
    seed: u64,
    tenant_count: u32,
    filter_modulus: u32,
    artifacts: [CertificationArtifactManifest; 2],
}

impl CertificationDatasetManifest {
    /// Returns the fixed 100k, 1M, and 10M certification manifests.
    #[must_use]
    pub const fn release_tiers() -> [Self; 3] {
        [Self::rows_100k(), Self::rows_1m(), Self::rows_10m()]
    }

    /// Returns the fixed 100k manifest.
    #[must_use]
    pub const fn rows_100k() -> Self {
        Self::new(
            CertificationTier::Rows100k,
            100_000,
            0x6374_785f_3130_306b,
            100,
            10,
            "certification/100k/raw-inputs-v1.bin",
            0xb7bd_18cf_35fe_8778,
            "certification/100k/exact-ground-truth-v1.bin",
            0x89d1_0ec6_d207_38c5,
        )
    }

    /// Returns the fixed 1M manifest.
    #[must_use]
    pub const fn rows_1m() -> Self {
        Self::new(
            CertificationTier::Rows1m,
            1_000_000,
            0x6374_785f_3031_6d00,
            1_000,
            20,
            "certification/1m/raw-inputs-v1.bin",
            0x3706_9ed6_2567_d8a6,
            "certification/1m/exact-ground-truth-v1.bin",
            0x359f_09e1_45f4_9a7f,
        )
    }

    /// Returns the fixed 10M manifest.
    #[must_use]
    pub const fn rows_10m() -> Self {
        Self::new(
            CertificationTier::Rows10m,
            10_000_000,
            0x6374_785f_3130_6d00,
            10_000,
            100,
            "certification/10m/raw-inputs-v1.bin",
            0xbe45_72d7_1948_c28f,
            "certification/10m/exact-ground-truth-v1.bin",
            0xc4bc_fbcb_530c_f16b,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "each fixed release tier pins two retained artifact identities"
    )]
    const fn new(
        tier: CertificationTier,
        rows: u64,
        seed: u64,
        tenant_count: u32,
        filter_modulus: u32,
        raw_path: &'static str,
        raw_checksum: u64,
        exact_path: &'static str,
        exact_checksum: u64,
    ) -> Self {
        Self {
            tier,
            rows,
            dimensions: 100,
            seed,
            tenant_count,
            filter_modulus,
            artifacts: [
                CertificationArtifactManifest {
                    kind: CertificationArtifactKind::RawInputs,
                    relative_path: raw_path,
                    checksum: raw_checksum,
                },
                CertificationArtifactManifest {
                    kind: CertificationArtifactKind::ExactGroundTruth,
                    relative_path: exact_path,
                    checksum: exact_checksum,
                },
            ],
        }
    }

    /// Returns the scale tier.
    #[must_use]
    pub const fn tier(self) -> CertificationTier {
        self.tier
    }

    /// Returns authoritative row cardinality.
    #[must_use]
    pub const fn rows(self) -> u64 {
        self.rows
    }

    /// Returns vector dimensions.
    #[must_use]
    pub const fn dimensions(self) -> u16 {
        self.dimensions
    }

    /// Returns the deterministic fixture seed.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Returns controlled tenant cardinality.
    #[must_use]
    pub const fn tenant_count(self) -> u32 {
        self.tenant_count
    }

    /// Returns the controlled selectivity modulus.
    #[must_use]
    pub const fn filter_modulus(self) -> u32 {
        self.filter_modulus
    }

    /// Returns the exact-oracle query count.
    #[must_use]
    pub const fn exact_query_count(self) -> u32 {
        EXACT_QUERY_COUNT
    }

    /// Returns the exact number of ranked neighbors retained for every query.
    #[must_use]
    pub const fn exact_k(self) -> usize {
        EXACT_K
    }

    /// Returns the generator format version.
    #[must_use]
    pub const fn generator_version(self) -> u16 {
        GENERATOR_VERSION
    }

    /// Returns both required retained artifacts.
    #[must_use]
    pub const fn artifacts(self) -> [CertificationArtifactManifest; 2] {
        self.artifacts
    }

    /// Returns the canonical exact-result artifact checksum.
    #[must_use]
    pub const fn exact_ground_truth_checksum(self) -> u64 {
        self.artifacts[1].checksum
    }

    /// Returns whether raw input/result artifacts must be retained.
    #[must_use]
    pub const fn retain_raw_artifacts(self) -> bool {
        true
    }

    /// Returns the retained vector seed for one authoritative point.
    #[must_use]
    pub fn vector_seed(self, point_id: u64) -> u64 {
        splitmix64(self.seed ^ point_id.saturating_sub(1))
    }

    /// Returns the controlled tenant value for one authoritative point.
    #[must_use]
    pub fn tenant_for_point(self, point_id: u64) -> u32 {
        u32::try_from(point_id.saturating_sub(1) % u64::from(self.tenant_count)).unwrap_or_default()
    }

    /// Returns the independently hashed controlled filter value for one point.
    #[must_use]
    pub fn filter_for_point(self, point_id: u64) -> u32 {
        filter_for_row(
            self.seed,
            point_id.saturating_sub(1),
            self.tenant_count,
            self.filter_modulus,
        )
    }

    /// Expands one deterministic vector component from the retained vector seed.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "the extracted mantissa is bounded to 23 bits and is exact in f32"
    )]
    pub fn vector_component(self, point_id: u64, dimension: u16) -> f32 {
        vector_component_from_seed(self.vector_seed(point_id), dimension)
    }

    /// Expands one component directly from a retained raw-input vector seed.
    #[must_use]
    pub fn expand_vector_seed_component(self, vector_seed: u64, dimension: u16) -> f32 {
        vector_component_from_seed(vector_seed, dimension)
    }

    /// Computes the canonical controlled-filter exact top-k for one query.
    ///
    /// Queries use an authoritative point vector and its tenant/filter values.
    /// Every eligible authoritative point is scored, then ordered by exact
    /// squared-L2 distance and point identifier for deterministic ties.
    #[must_use]
    pub fn exact_neighbors(self, query: u32) -> Vec<ExactNeighbor> {
        exact_neighbors(
            self.rows,
            self.dimensions,
            self.seed,
            self.tenant_count,
            self.filter_modulus,
            query,
            EXACT_K,
        )
    }

    /// Streams one canonical retained artifact and returns its checksum.
    ///
    /// # Errors
    ///
    /// Returns writer failures without claiming a complete checksum.
    pub fn generate_artifact(
        self,
        kind: CertificationArtifactKind,
        mut writer: impl Write,
    ) -> io::Result<u64> {
        let mut checksum = FNV_OFFSET;
        match kind {
            CertificationArtifactKind::RawInputs => {
                for row in 0..self.rows {
                    let point_id = row + 1;
                    let tenant = self.tenant_for_point(point_id);
                    let filter = self.filter_for_point(point_id);
                    let vector_seed = self.vector_seed(point_id);
                    write_checksummed(&mut writer, &mut checksum, &point_id.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &tenant.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &filter.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &vector_seed.to_le_bytes())?;
                }
            }
            CertificationArtifactKind::ExactGroundTruth => {
                for query in 0..EXACT_QUERY_COUNT {
                    let query_point_id = query_point_id(self.seed, self.rows, query);
                    let tenant = self.tenant_for_point(query_point_id);
                    let filter = self.filter_for_point(query_point_id);
                    let query_seed = self.vector_seed(query_point_id);
                    let neighbors = self.exact_neighbors(query);
                    write_checksummed(&mut writer, &mut checksum, &query.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &query_seed.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &tenant.to_le_bytes())?;
                    write_checksummed(&mut writer, &mut checksum, &filter.to_le_bytes())?;
                    write_checksummed(
                        &mut writer,
                        &mut checksum,
                        &u16::try_from(neighbors.len())
                            .unwrap_or_default()
                            .to_le_bytes(),
                    )?;
                    for neighbor in neighbors {
                        write_checksummed(
                            &mut writer,
                            &mut checksum,
                            &neighbor.point_id.to_le_bytes(),
                        )?;
                        write_checksummed(
                            &mut writer,
                            &mut checksum,
                            &neighbor.squared_l2.to_le_bytes(),
                        )?;
                    }
                }
            }
        }
        Ok(checksum)
    }

    /// Verifies retained bytes against the manifest inventory.
    ///
    /// # Errors
    ///
    /// Returns read failures or `InvalidData` for a checksum mismatch.
    pub fn verify_artifact(
        self,
        kind: CertificationArtifactKind,
        mut reader: impl Read,
    ) -> io::Result<()> {
        let mut checksum = FNV_OFFSET;
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            checksum = checksum_bytes(checksum, &buffer[..read]);
        }
        let expected = self
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .map(|artifact| artifact.checksum)
            .unwrap_or_default();
        if checksum == expected {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "certification artifact checksum mismatch",
            ))
        }
    }
}

fn query_point_id(seed: u64, rows: u64, query: u32) -> u64 {
    splitmix64(seed ^ EXACT_DOMAIN ^ u64::from(query)) % rows + 1
}

fn exact_neighbors(
    rows: u64,
    dimensions: u16,
    seed: u64,
    tenant_count: u32,
    filter_modulus: u32,
    query: u32,
    k: usize,
) -> Vec<ExactNeighbor> {
    let query_point_id = query_point_id(seed, rows, query);
    let query_row = query_point_id - 1;
    let tenant = query_row % u64::from(tenant_count);
    let filter = filter_for_row(seed, query_row, tenant_count, filter_modulus);
    let query_seed = splitmix64(seed ^ query_row);
    let mut neighbors = Vec::new();
    let mut row = tenant;
    while row < rows {
        if filter_for_row(seed, row, tenant_count, filter_modulus) == filter {
            let point_seed = splitmix64(seed ^ row);
            neighbors.push(ExactNeighbor {
                point_id: row + 1,
                squared_l2: squared_l2(query_seed, point_seed, dimensions),
            });
        }
        row = row.saturating_add(u64::from(tenant_count));
    }
    neighbors.sort_unstable_by(|left, right| {
        left.squared_l2
            .total_cmp(&right.squared_l2)
            .then_with(|| left.point_id.cmp(&right.point_id))
    });
    neighbors.truncate(k);
    neighbors
}

fn filter_for_row(seed: u64, row: u64, tenant_count: u32, filter_modulus: u32) -> u32 {
    let tenant = row % u64::from(tenant_count);
    let tenant_position = row / u64::from(tenant_count);
    let rotation = splitmix64(seed ^ FILTER_DOMAIN ^ tenant) % u64::from(filter_modulus);
    u32::try_from((tenant_position + rotation) % u64::from(filter_modulus)).unwrap_or_default()
}

fn squared_l2(left_seed: u64, right_seed: u64, dimensions: u16) -> f64 {
    (0..dimensions)
        .map(|dimension| {
            let delta = f64::from(vector_component_from_seed(left_seed, dimension))
                - f64::from(vector_component_from_seed(right_seed, dimension));
            delta * delta
        })
        .sum()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the extracted mantissa is bounded to 23 bits and is exact in f32"
)]
fn vector_component_from_seed(vector_seed: u64, dimension: u16) -> f32 {
    let counter = splitmix64(
        vector_seed ^ VECTOR_DOMAIN ^ (u64::from(dimension) << 32) ^ u64::from(GENERATOR_VERSION),
    );
    let mantissa = u32::try_from(counter >> 41).unwrap_or_default();
    (mantissa as f32) / ((1_u32 << 23) as f32)
}

fn write_checksummed(writer: &mut impl Write, checksum: &mut u64, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)?;
    *checksum = checksum_bytes(*checksum, bytes);
    Ok(())
}

fn checksum_bytes(initial: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(initial, |checksum, byte| {
        (checksum ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
