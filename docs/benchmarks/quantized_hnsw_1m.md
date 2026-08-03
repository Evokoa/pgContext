# Quantized HNSW one-million-row certification

Phase 5 certifies binary, scalar/SQ8, and product-quantized HNSW serving against
the frozen manifest `d40fe51a782cd221`. The same release build and workload
passed on PostgreSQL 17 and 18.

## Frozen workload

- 1,000,000 authoritative `vector(32)` rows generated independently per
  dimension with generator revision 2 and seed `5779599704526766897`;
- five frozen source-row queries (`17`, `200003`, `400009`, `700001`, and
  `999983`) with exact top-10 ground truth;
- deterministic codec training from at most 4,096 evenly distributed rows;
- `hnsw_ef_search = 4096`, candidate and iterative-expansion limits of 10,000;
- four bounded segment workers, with a mandatory parallel ten-segment scan and
  aggregate packed-generation memory accounting;
- one unmeasured cold query to publish the committed packed generation,
  followed by five warm latency samples;
- VACUUM, full one-million-row REINDEX, exact-query verification, server
  immediate stop, WAL/crash recovery verification, and generation retirement
  for every codec.

The executable gate is `tests/heavy/quantized_hnsw_1m.sh`; its thresholds are
owned by `crates/context-test/src/p5_codec.rs`. The performance lane requires a
release-built extension and refuses a mismatched manifest when resuming.

## Environment

- Apple M4 Pro, arm64, 24 GiB RAM;
- macOS Darwin 25.5.0;
- PostgreSQL 17.10 and PostgreSQL 18.4;
- Rust 1.96.0 and pgrx 0.19.1;
- release profile.

## Results

All byte counts are exact. Resident bytes are the complete concurrently loaded
ten-segment packed generation, not one segment. Latency is warm p95 across the
five frozen queries; recall is the minimum exact-oracle top-10 recall across
those queries.

| PostgreSQL | Codec | Build (s) | Index bytes | Resident bytes | Minimum recall | Warm p95 (ms) | Published | VACUUM/REINDEX | Crash recovery |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 17.10 | binary | 343 | 344,637,440 | 421,429,328 | 1.00 | 3,365.485 | yes | pass | pass |
| 17.10 | scalar | 345 | 344,637,440 | 437,429,328 | 1.00 | 3,358.074 | yes | pass | pass |
| 17.10 | pq | 328 | 344,637,440 | 422,003,728 | 1.00 | 3,921.433 | yes | pass | pass |
| 18.4 | binary | 352 | 344,637,440 | 421,429,328 | 1.00 | 3,243.751 | yes | pass | pass |
| 18.4 | scalar | 348 | 344,637,440 | 437,429,328 | 1.00 | 3,428.107 | yes | pass | pass |
| 18.4 | pq | 350 | 344,637,440 | 422,003,728 | 1.00 | 4,046.847 | yes | pass | pass |

Frozen promotion thresholds are minimum recall 0.70/0.95/0.90 for
binary/scalar/PQ, warm p95 at most 5,000 ms, resident size at most 2 GiB, index
size at most 4 GiB, build time at most 7,200 seconds, and successful
publication, VACUUM/REINDEX, and immediate-stop recovery. Every row passed.

This result certifies the Phase 5 one-million-row arm64 lane. It does not stand
in for the separate 10M, x86-64, concurrency, replica, or broad production
certification program.
