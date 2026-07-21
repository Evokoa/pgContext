#!/usr/bin/env python3
"""Reproducible end-to-end pgContext, pgvector, and Qdrant benchmark."""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import random
import ssl
import statistics
import subprocess
import time
import urllib.request
from pathlib import Path
from typing import Iterable, Sequence

import numpy as np
import psycopg
from qdrant_client import QdrantClient, models
import certifi
from fastembed import TextEmbedding

MODEL = "sentence-transformers/all-MiniLM-L6-v2"
MODEL_DIMENSIONS = 384
DATASET = "mteb/scifact"
DATASET_REVISION = "cf10ab6856b15b0e670ef8ae5dae4e266c12d035"
DATASET_BASE = f"https://huggingface.co/datasets/{DATASET}/resolve/{DATASET_REVISION}"
SEED = 20260715
TOP_K = 10
HNSW_M = 16
# Overridable so a diagnostic run can vary construction effort without editing
# the harness. The value lands in every report's `configuration` block, so a
# result file always says which setting produced it.
HNSW_EF_CONSTRUCTION = int(os.environ.get("PGCONTEXT_BENCH_EF_CONSTRUCTION", "64"))
# Unset leaves pgcontext.hnsw_build_parallel_workers at its own default; set to
# 1 to force a serial build when comparing graph quality against the parallel
# builder.
PGCONTEXT_BUILD_WORKERS = os.environ.get("PGCONTEXT_BENCH_BUILD_WORKERS", "")
# Restricting this to one system is what makes A/B diagnostics affordable:
# comparing pgContext against itself does not need pgvector reloaded and
# rebuilt on every arm.
BENCH_SYSTEMS = tuple(
    name.strip()
    for name in os.environ.get("PGCONTEXT_BENCH_SYSTEMS", "pgcontext,pgvector").split(",")
    if name.strip()
)
# Qdrant is a separate service rather than a PostgreSQL database, so it is
# selected separately. Default on, to leave comparative runs unchanged; a
# self-comparison sets PGCONTEXT_BENCH_SYSTEMS without it and skips the
# service entirely instead of failing when it is not running.
BENCH_QDRANT = os.environ.get("PGCONTEXT_BENCH_SKIP_QDRANT", "") != "1" and "qdrant" in tuple(
    name.strip()
    for name in os.environ.get(
        "PGCONTEXT_BENCH_SYSTEMS", "pgcontext,pgvector,qdrant"
    ).split(",")
    if name.strip()
)
PGCONTEXT_HNSW_EF_SEARCH = int(os.environ.get("PGCONTEXT_HNSW_EF_SEARCH", "40"))
PGVECTOR_HNSW_EF_SEARCH = int(os.environ.get("PGVECTOR_HNSW_EF_SEARCH", "40"))
QDRANT_HNSW_EF_SEARCH = int(os.environ.get("QDRANT_HNSW_EF_SEARCH", "40"))
QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
QDRANT_GRPC_PORT = int(os.environ.get("QDRANT_GRPC_PORT", "6334"))
QDRANT_COLLECTION = "pgcontext_benchmark"
# Skip the Qdrant lanes entirely (for CI runners without the service).
SKIP_QDRANT = os.environ.get("PGCONTEXT_BENCH_SKIP_QDRANT", "") == "1"
# Mirrors context_core::policy::MAX_HNSW_CANDIDATE_MASK_POINTS: the engine
# rejects masked traversal above this many mask TIDs, so lanes whose mask
# would exceed it are skipped and recorded rather than crashed.
MASKED_POINT_BUDGET = 10_000
DEFAULT_QUERIES = 200
DEFAULT_WARMUP = 20
# Index-build budget applied identically to both PostgreSQL systems. pgContext
# refuses builds whose estimated memory exceeds maintenance_work_mem instead of
# degrading, so scale lanes need an explicit budget (PostgreSQL defaults to
# 64MB, which caps out near 100k 384-dimensional rows).
MAINTENANCE_WORK_MEM = os.environ.get("PGCONTEXT_BENCH_MAINTENANCE_WORK_MEM", "2GB")
DEFAULT_SWEEP_EF_VALUES = (16, 24, 32, 48, 64, 96)
# Filter lanes keyed by selectivity label: (column/payload field, modulo).
# A predicate `column = index % modulo` matches 1/modulo of the corpus.
SELECTIVITY_LANES = {
    "1_percent": ("bucket_100", 100),
    "10_percent": ("tenant_id", 10),
    "50_percent": ("bucket_2", 2),
}
FILTER_MODULO = {column: modulo for column, modulo in SELECTIVITY_LANES.values()}
SYNTHETIC_CLUSTERS = 64
SYNTHETIC_QUERY_ROWS = 1000


def percentile(values: Sequence[float], quantile: float) -> float:
    """Return a linearly interpolated percentile for a non-empty sample."""
    if not values:
        raise ValueError("percentile requires at least one value")
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile / 100
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def mean_recall_at_k(
    exact: Sequence[Sequence[int]], approximate: Sequence[Sequence[int]], k: int
) -> float:
    """Return mean set-overlap recall at k."""
    if len(exact) != len(approximate) or not exact:
        raise ValueError("recall inputs must be non-empty and have equal length")
    return statistics.fmean(
        len(set(expected[:k]).intersection(actual[:k])) / k
        for expected, actual in zip(exact, approximate, strict=True)
    )


def vector_literal(values: Iterable[float]) -> str:
    """Serialize finite floats to the text format accepted by both extensions."""
    rendered = []
    for value in values:
        number = float(value)
        if not math.isfinite(number):
            raise ValueError("vectors must contain only finite values")
        rendered.append(format(number, ".9g"))
    return "[" + ",".join(rendered) + "]"


def fetch_jsonl(filename: str) -> list[dict[str, object]]:
    tls = ssl.create_default_context(cafile=certifi.where())
    request = urllib.request.Request(
        f"{DATASET_BASE}/{filename}", headers={"User-Agent": "pgcontext-benchmark/0.1"}
    )
    with urllib.request.urlopen(request, timeout=120, context=tls) as response:
        return [json.loads(line) for line in response if line.strip()]


def prepare_data(output_dir: Path) -> dict[str, object]:
    output_dir.mkdir(parents=True, exist_ok=True)
    corpus_rows = fetch_jsonl("corpus.jsonl")
    query_rows = fetch_jsonl("queries.jsonl")
    corpus_text = [f"{row['title']} {row['text']}" for row in corpus_rows]
    query_text = [str(row["text"]) for row in query_rows]

    model = TextEmbedding(model_name=MODEL)
    corpus_vectors = np.asarray(list(model.embed(corpus_text, batch_size=64)), dtype=np.float32)
    query_vectors = np.asarray(list(model.query_embed(query_text, batch_size=64)), dtype=np.float32)
    if corpus_vectors.shape[1] != MODEL_DIMENSIONS or query_vectors.shape[1] != MODEL_DIMENSIONS:
        raise RuntimeError("embedding model returned an unexpected dimension")

    np.save(output_dir / "corpus.npy", corpus_vectors)
    np.save(output_dir / "queries.npy", query_vectors)
    metadata = {
        "dataset": DATASET,
        "dataset_revision": DATASET_REVISION,
        "corpus_rows": len(corpus_rows),
        "query_rows": len(query_rows),
        "model": MODEL,
        "dimensions": MODEL_DIMENSIONS,
        "seed": SEED,
    }
    (output_dir / "dataset.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return metadata


def reset_database(admin_dsn: str, database: str) -> None:
    with psycopg.connect(admin_dsn, autocommit=True) as connection:
        connection.execute(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity "
            "WHERE datname = %s AND pid <> pg_backend_pid()",
            (database,),
        )
        connection.execute(f'DROP DATABASE IF EXISTS "{database}"')
        connection.execute(f'CREATE DATABASE "{database}"')


def database_dsn(admin_dsn: str, database: str) -> str:
    info = psycopg.conninfo.conninfo_to_dict(admin_dsn)
    info["dbname"] = database
    return psycopg.conninfo.make_conninfo(**info)


def load_system(
    admin_dsn: str, system: str, vectors: np.ndarray
) -> tuple[str, float, float, int, int]:
    database = f"pgcontext_bench_{system}"
    reset_database(admin_dsn, database)
    dsn = database_dsn(admin_dsn, database)
    extension = "pgcontext" if system == "pgcontext" else "vector"
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute(f"CREATE EXTENSION {extension}")
        connection.execute(
            f"CREATE TABLE items (id bigint PRIMARY KEY, tenant_id integer NOT NULL, "
            f"bucket_100 integer NOT NULL, bucket_2 integer NOT NULL, "
            f"embedding vector({MODEL_DIMENSIONS}) NOT NULL)"
        )
        started = time.perf_counter()
        with connection.cursor().copy(
            "COPY items (id, tenant_id, bucket_100, bucket_2, embedding) FROM STDIN"
        ) as copy:
            for index, vector in enumerate(vectors, start=1):
                copy.write_row(
                    (index, index % 10, index % 100, index % 2, vector_literal(vector))
                )
        load_seconds = time.perf_counter() - started
        connection.execute("ANALYZE items")

        connection.execute(f"SET maintenance_work_mem = '{MAINTENANCE_WORK_MEM}'")
        if system == "pgcontext":
            connection.execute(f"SET pgcontext.hnsw_m = {HNSW_M}")
            connection.execute(
                f"SET pgcontext.hnsw_ef_construction = {HNSW_EF_CONSTRUCTION}"
            )
            if PGCONTEXT_BUILD_WORKERS:
                connection.execute(
                    "SET pgcontext.hnsw_build_parallel_workers = "
                    f"{int(PGCONTEXT_BUILD_WORKERS)}"
                )
            create_index = (
                "CREATE INDEX items_embedding_hnsw ON items USING pgcontext_hnsw "
                "(embedding pgcontext.vector_hnsw_cosine_ops)"
            )
        else:
            create_index = (
                "CREATE INDEX items_embedding_hnsw ON items USING hnsw "
                f"(embedding vector_cosine_ops) WITH (m={HNSW_M}, "
                f"ef_construction={HNSW_EF_CONSTRUCTION})"
            )
        started = time.perf_counter()
        connection.execute(create_index)
        build_seconds = time.perf_counter() - started
        connection.execute("ANALYZE items")
        table_bytes = connection.execute(
            "SELECT pg_relation_size('items')"
        ).fetchone()[0]
        index_bytes = connection.execute(
            "SELECT pg_relation_size('items_embedding_hnsw')"
        ).fetchone()[0]
    return dsn, load_seconds, build_seconds, table_bytes, index_bytes


def qdrant_client() -> QdrantClient:
    return QdrantClient(
        url=QDRANT_URL,
        grpc_port=QDRANT_GRPC_PORT,
        prefer_grpc=True,
        timeout=120,
    )


def load_qdrant(vectors: np.ndarray) -> tuple[QdrantClient, float, float]:
    """Load points without HNSW, then time Qdrant's segment index build."""
    client = qdrant_client()
    if client.collection_exists(QDRANT_COLLECTION):
        client.delete_collection(QDRANT_COLLECTION)
    client.create_collection(
        collection_name=QDRANT_COLLECTION,
        vectors_config=models.VectorParams(
            size=MODEL_DIMENSIONS,
            distance=models.Distance.COSINE,
            hnsw_config=models.HnswConfigDiff(
                m=HNSW_M,
                ef_construct=HNSW_EF_CONSTRUCTION,
            ),
        ),
        optimizers_config=models.OptimizersConfigDiff(indexing_threshold=0),
    )
    # Create the payload index before vector indexing so Qdrant can incorporate
    # filter-aware edges when the segment optimizer builds HNSW.
    for field_name in FILTER_MODULO:
        client.create_payload_index(
            collection_name=QDRANT_COLLECTION,
            field_name=field_name,
            field_schema=models.PayloadSchemaType.INTEGER,
            wait=True,
        )
    started = time.perf_counter()
    client.upload_collection(
        collection_name=QDRANT_COLLECTION,
        vectors=vectors.tolist(),
        ids=list(range(1, len(vectors) + 1)),
        payload=[
            {"tenant_id": index % 10, "bucket_100": index % 100, "bucket_2": index % 2}
            for index in range(1, len(vectors) + 1)
        ],
        batch_size=256,
        parallel=1,
        wait=True,
    )
    load_seconds = time.perf_counter() - started

    started = time.perf_counter()
    client.update_collection(
        collection_name=QDRANT_COLLECTION,
        optimizers_config=models.OptimizersConfigDiff(indexing_threshold=1),
    )
    # 1M-row corpora need well over the old 120 s ceiling on a busy host
    # (observed: 714k/1M indexed at the 120 s mark); the wait is part of
    # Qdrant's measured build time either way, so a generous ceiling only
    # guards against a truly hung optimizer.
    optimization_ceiling_seconds = 900
    deadline = time.monotonic() + optimization_ceiling_seconds
    while True:
        collection = client.get_collection(QDRANT_COLLECTION)
        status = str(collection.status).lower()
        indexed = int(collection.indexed_vectors_count or 0)
        if status.endswith("green") and indexed >= len(vectors):
            break
        if time.monotonic() >= deadline:
            raise RuntimeError(
                "Qdrant did not finish HNSW optimization within "
                f"{optimization_ceiling_seconds} seconds "
                f"(status={collection.status}, indexed={indexed}/{len(vectors)})"
            )
        time.sleep(0.05)
    build_seconds = time.perf_counter() - started
    return client, load_seconds, build_seconds


def qdrant_effective_candidates(ef: int, segments_count: int) -> int:
    """Upper bound on the candidates one Qdrant query explores at `hnsw_ef=ef`.

    Qdrant applies `hnsw_ef` per segment and searches every segment of the
    collection, merging the results, so the nominal ef understates total
    search effort by the segment count. Comparing a segmented system against
    a single-graph system at "matched ef" is therefore effort-mismatched by
    roughly this factor -- the misreading behind the retracted 1M
    graph-quality conclusion in docs/benchmarks/pgvector.md. Effort-matched
    comparisons should use this value, not the nominal ef.
    """
    return ef * max(1, int(segments_count))


def qdrant_effort_metadata(client: QdrantClient, ef_values: Sequence[int]) -> dict[str, object]:
    """Records the segment count and per-ef effective candidate totals.

    Failure degrades to a recorded error rather than killing the lane: the
    numbers already measured are worth keeping even if this follow-up call
    races a collection teardown.
    """
    try:
        info = client.get_collection(QDRANT_COLLECTION)
        segments = int(info.segments_count or 0)
    except Exception as error:  # noqa: BLE001 - service-dependent, recorded not raised
        return {"segments_count": None, "capture_error": str(error)}
    return {
        "segments_count": segments,
        "ef_semantics": "hnsw_ef applies per segment; all segments searched and merged",
        "effective_candidates": {
            str(ef): qdrant_effective_candidates(ef, segments) for ef in ef_values
        },
    }


def execute_qdrant_queries(
    client: QdrantClient,
    query_vectors: Sequence[np.ndarray],
    approximate: bool,
    filter_field: str | None,
    warmup: int,
    ef_search: int | None = None,
) -> tuple[list[list[int]], list[float], str]:
    queries = [vector.tolist() for vector in query_vectors]
    modulo = FILTER_MODULO[filter_field] if filter_field else 0
    filters = [
        models.Filter(
            must=[
                models.FieldCondition(
                    key=filter_field,
                    match=models.MatchValue(value=index % modulo),
                )
            ]
        )
        if filter_field
        else None
        for index in range(len(queries))
    ]
    search_params = models.SearchParams(
        hnsw_ef=ef_search if ef_search is not None else QDRANT_HNSW_EF_SEARCH,
        exact=not approximate,
    )

    def query(index: int) -> list[int]:
        response = client.query_points(
            collection_name=QDRANT_COLLECTION,
            query=queries[index],
            query_filter=filters[index],
            search_params=search_params,
            limit=TOP_K,
            with_payload=False,
            with_vectors=False,
        )
        return [int(point.id) for point in response.points]

    for index in range(min(warmup, len(queries))):
        query(index)
    results: list[list[int]] = []
    latencies_ms: list[float] = []
    for index in range(len(queries)):
        started = time.perf_counter_ns()
        results.append(query(index))
        latencies_ms.append((time.perf_counter_ns() - started) / 1_000_000)
    mode = "HNSW" if approximate else "exact"
    predicate = f" with {filter_field} payload filter" if filter_field else ""
    return results, latencies_ms, f"Qdrant Query API {mode}{predicate}"


def query_sql(system: str, approximate: bool, filter_column: str | None) -> str:
    operator = "OPERATOR(pgcontext.<=>)" if system == "pgcontext" else "<=>"
    predicate = f"WHERE {filter_column} = %s " if filter_column else ""
    return (
        f"SELECT id FROM items {predicate}ORDER BY embedding {operator} %s::vector "
        f"LIMIT {TOP_K}"
    )


def execute_queries(
    dsn: str,
    system: str,
    query_vectors: Sequence[np.ndarray],
    approximate: bool,
    filter_column: str | None,
    warmup: int,
    ef_search: int | None = None,
) -> tuple[list[list[int]], list[float], str]:
    sql = query_sql(system, approximate, filter_column)
    results: list[list[int]] = []
    latencies_ms: list[float] = []
    modulo = FILTER_MODULO[filter_column] if filter_column else 0

    # Text serialization is client-side benchmark setup, not database query
    # execution. Preparing it here keeps Python float formatting out of both
    # systems' measured latency windows.
    query_parameters = [
        (index % modulo, vector_literal(vector))
        if filter_column
        else (vector_literal(vector),)
        for index, vector in enumerate(query_vectors)
    ]
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute("SET jit = off")
        connection.execute("SET max_parallel_workers_per_gather = 0")
        if approximate:
            connection.execute("SET enable_seqscan = off")
            if system == "pgcontext":
                effective_ef = (
                    ef_search if ef_search is not None else PGCONTEXT_HNSW_EF_SEARCH
                )
                connection.execute(
                    f"SET pgcontext.hnsw_ef_search = {effective_ef}"
                )
            else:
                effective_ef = (
                    ef_search if ef_search is not None else PGVECTOR_HNSW_EF_SEARCH
                )
                connection.execute(f"SET hnsw.ef_search = {effective_ef}")
                if filter_column:
                    connection.execute("SET hnsw.iterative_scan = strict_order")
        else:
            connection.execute("SET enable_indexscan = off")
            connection.execute("SET enable_bitmapscan = off")

        for parameters in query_parameters[:warmup]:
            connection.execute(sql, parameters).fetchall()
        for parameters in query_parameters:
            started = time.perf_counter_ns()
            rows = connection.execute(sql, parameters).fetchall()
            latencies_ms.append((time.perf_counter_ns() - started) / 1_000_000)
            results.append([int(row[0]) for row in rows])

        explain = "EXPLAIN (COSTS OFF) " + sql
        plan = "\n".join(
            row[0]
            for row in connection.execute(
                explain, query_parameters[0]
            ).fetchall()
        )
        if approximate and "Index Scan using items_embedding_hnsw" not in plan:
            raise RuntimeError(f"{system} ANN query did not use HNSW:\n{plan}")
    return results, latencies_ms, plan


def execute_pgcontext_masked_queries(
    dsn: str,
    query_vectors: Sequence[np.ndarray],
    warmup: int,
    filter_column: str = "tenant_id",
    ef_search: int | None = None,
) -> tuple[list[list[int]], list[float], str]:
    """Measure pgContext's filter-aware masked traversal on the same table."""
    modulo = FILTER_MODULO[filter_column]
    sql = (
        "WITH candidate_mask AS MATERIALIZED ("
        "SELECT array_agg(ctid ORDER BY ctid) AS heap_tids "
        f"FROM items WHERE {filter_column} = %s"
        ") "
        "SELECT items.id "
        "FROM candidate_mask "
        "CROSS JOIN LATERAL pgcontext._hnsw_masked_candidates("
        "'items_embedding_hnsw'::regclass, %s::vector, "
        f"candidate_mask.heap_tids, {TOP_K}"
        ") AS ann "
        "JOIN items ON items.ctid = ann.heap_tid::tid "
        "ORDER BY ann.score, items.id "
        f"LIMIT {TOP_K}"
    )
    query_parameters = [
        (index % modulo, vector_literal(vector))
        for index, vector in enumerate(query_vectors)
    ]
    results: list[list[int]] = []
    latencies_ms: list[float] = []
    effective_ef = ef_search if ef_search is not None else PGCONTEXT_HNSW_EF_SEARCH
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute("SET jit = off")
        connection.execute("SET max_parallel_workers_per_gather = 0")
        connection.execute(f"SET pgcontext.hnsw_ef_search = {effective_ef}")
        for parameters in query_parameters[:warmup]:
            connection.execute(sql, parameters).fetchall()
        for parameters in query_parameters:
            started = time.perf_counter_ns()
            rows = connection.execute(sql, parameters).fetchall()
            latencies_ms.append((time.perf_counter_ns() - started) / 1_000_000)
            results.append([int(row[0]) for row in rows])
        plan = "\n".join(
            row[0]
            for row in connection.execute(
                "EXPLAIN (COSTS OFF) " + sql, query_parameters[0]
            ).fetchall()
        )
        if "_hnsw_masked_candidates" not in plan:
            raise RuntimeError(f"pgContext masked query missed its traversal function:\n{plan}")
    return results, latencies_ms, plan


def setup_pgcontext_collection(dsn: str) -> None:
    """Register the loaded items table as a collection for adaptive search."""
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute(
            "SELECT * FROM pgcontext.create_collection('bench_items', 'public.items')"
        )
        connection.execute(
            "SELECT pgcontext.register_vector("
            f"'bench_items', 'embedding', 'embedding', {MODEL_DIMENSIONS}, 'cosine')"
        )
        for column in FILTER_MODULO:
            connection.execute(
                "SELECT pgcontext.register_filter_column("
                f"'bench_items', '{column}', '{column}')"
            )
        connection.execute(
            "SELECT pgcontext.bulk_upsert_points("
            "'bench_items', ARRAY(SELECT id::text FROM items ORDER BY id), 5000)"
        )


def execute_pgcontext_collection_queries(
    dsn: str,
    query_vectors: Sequence[np.ndarray],
    filter_column: str,
    warmup: int,
) -> tuple[list[list[int]], list[float]]:
    """Measure the registered-collection API, which self-selects its filtered
    strategy (exact for selective predicates, masked traversal otherwise)."""
    modulo = FILTER_MODULO[filter_column]
    # pgcontext.search's filter parameter is TEXT (a JSON-syntax string
    # parsed internally), not jsonb -- there is no (text, vector, jsonb,
    # int) overload, so casting to ::jsonb here raises UndefinedFunction.
    sql = (
        "SELECT source_key FROM pgcontext.search("
        f"'bench_items', %s::vector, %s::text, {TOP_K})"
    )
    query_parameters = [
        (
            vector_literal(vector),
            json.dumps({"must": [{"key": filter_column, "match": index % modulo}]}),
        )
        for index, vector in enumerate(query_vectors)
    ]
    results: list[list[int]] = []
    latencies_ms: list[float] = []
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute("SET jit = off")
        connection.execute("SET max_parallel_workers_per_gather = 0")
        connection.execute(f"SET pgcontext.hnsw_ef_search = {PGCONTEXT_HNSW_EF_SEARCH}")
        for parameters in query_parameters[:warmup]:
            connection.execute(sql, parameters).fetchall()
        for parameters in query_parameters:
            started = time.perf_counter_ns()
            rows = connection.execute(sql, parameters).fetchall()
            latencies_ms.append((time.perf_counter_ns() - started) / 1_000_000)
            results.append([int(row[0]) for row in rows])
    return results, latencies_ms


def summarize_latency(latencies_ms: Sequence[float]) -> dict[str, float]:
    return {
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "mean_ms": statistics.fmean(latencies_ms),
        "qps": 1000 / statistics.fmean(latencies_ms),
    }


def system_metadata(admin_dsn: str) -> dict[str, object]:
    with psycopg.connect(admin_dsn) as connection:
        postgres_version = connection.execute("SELECT version()").fetchone()[0]
    cpu = subprocess.run(
        ["sysctl", "-n", "machdep.cpu.brand_string"],
        check=False,
        capture_output=True,
        text=True,
    ).stdout.strip()
    return {
        "postgres": postgres_version,
        "platform": platform.platform(),
        "cpu": cpu or platform.processor(),
        "python": platform.python_version(),
        "git_sha": subprocess.run(
            ["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True
        ).stdout.strip(),
        "git_dirty": bool(
            subprocess.run(
                ["git", "status", "--porcelain"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        ),
    }


def run_benchmark(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
    trial_count: int,
) -> dict[str, object]:
    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    queries = [all_queries[index] for index in indexes]
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "dataset": DATASET,
            "corpus_rows": len(corpus),
            "queries": sample_count,
            "warmup": warmup,
            "trials": trial_count,
            "model": MODEL,
            "dimensions": MODEL_DIMENSIONS,
            "metric": "cosine",
            "top_k": TOP_K,
            "seed": SEED,
            "hnsw_m": HNSW_M,
            "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
            "pgcontext_build_parallel_workers": PGCONTEXT_BUILD_WORKERS or "default",
            "systems_measured": list(BENCH_SYSTEMS),
            "pgcontext_hnsw_ef_search": PGCONTEXT_HNSW_EF_SEARCH,
            "pgvector_hnsw_ef_search": PGVECTOR_HNSW_EF_SEARCH,
            "qdrant_hnsw_ef_search": QDRANT_HNSW_EF_SEARCH,
        },
        "systems": {},
        "trials": [],
    }
    accumulators: dict[str, dict[str, object]] = {
        system: {
            "load_seconds": [],
            "build_seconds": [],
            "table_bytes": [],
            "index_bytes": [],
            "exact_latency": [],
            "ann_latency": [],
            "ann_recall": [],
            "filtered_exact_latency": [],
            "filtered_ann_latency": [],
            "filtered_ann_recall": [],
            "filtered_full_result_rate": [],
            "masked_latency": [],
            "masked_recall": [],
            "masked_full_result_rate": [],
            "ann_plan": "",
            "filtered_plan": "",
            "masked_plan": "",
        }
        for system in ("pgcontext", "pgvector", "qdrant")
        if not (SKIP_QDRANT and system == "qdrant")
    }
    for trial_index in range(trial_count):
        systems = (
            BENCH_SYSTEMS
            if SKIP_QDRANT
            else ("pgcontext", "pgvector", "qdrant")
        )
        rotation = trial_index % len(systems)
        order = systems[rotation:] + systems[:rotation]
        trial_report: dict[str, object] = {
            "trial": trial_index + 1,
            "order": list(order),
            "systems": {},
        }
        for system in order:
            prefix = f"[trial {trial_index + 1}/{trial_count}] [{system}]"
            print(f"{prefix} loading {len(corpus)} vectors", flush=True)
            if system == "qdrant":
                client, load_seconds, build_seconds = load_qdrant(corpus)
                table_bytes = None
                index_bytes = None
                print(f"{prefix} exact queries", flush=True)
                exact, exact_latency, _ = execute_qdrant_queries(
                    client, queries, False, None, warmup
                )
                print(f"{prefix} HNSW queries", flush=True)
                ann, ann_latency, ann_plan = execute_qdrant_queries(
                    client, queries, True, None, warmup
                )
                print(f"{prefix} exact filtered queries", flush=True)
                filtered_exact, filtered_exact_latency, _ = execute_qdrant_queries(
                    client, queries, False, "tenant_id", warmup
                )
                print(f"{prefix} HNSW filtered queries", flush=True)
                filtered_ann, filtered_ann_latency, filtered_plan = (
                    execute_qdrant_queries(client, queries, True, "tenant_id", warmup)
                )
                client.close()
                dsn = ""
            else:
                dsn, load_seconds, build_seconds, table_bytes, index_bytes = load_system(
                    admin_dsn, system, corpus
                )
                print(f"{prefix} exact queries", flush=True)
                exact, exact_latency, _ = execute_queries(
                    dsn, system, queries, False, None, warmup
                )
                print(f"{prefix} HNSW queries", flush=True)
                ann, ann_latency, ann_plan = execute_queries(
                    dsn, system, queries, True, None, warmup
                )
                print(f"{prefix} exact filtered queries", flush=True)
                filtered_exact, filtered_exact_latency, _ = execute_queries(
                    dsn, system, queries, False, "tenant_id", warmup
                )
                print(f"{prefix} HNSW filtered queries", flush=True)
                filtered_ann, filtered_ann_latency, filtered_plan = execute_queries(
                    dsn, system, queries, True, "tenant_id", warmup
                )
            ann_recall = mean_recall_at_k(exact, ann, TOP_K)
            filtered_ann_recall = mean_recall_at_k(
                filtered_exact, filtered_ann, TOP_K
            )
            full_result_rate = statistics.fmean(
                len(rows) == TOP_K for rows in filtered_ann
            )
            masked_latency: list[float] = []
            masked_recall = 0.0
            masked_full_result_rate = 0.0
            masked_plan = ""
            if system == "pgcontext":
                mask_rows = len(corpus) // 10
                if mask_rows > MASKED_POINT_BUDGET:
                    print(
                        f"{prefix} skipping masked lane: {mask_rows}-row mask "
                        f"exceeds the engine's {MASKED_POINT_BUDGET}-point budget",
                        flush=True,
                    )
                    masked_plan = (
                        f"skipped: mask of {mask_rows} rows exceeds "
                        f"MAX_HNSW_CANDIDATE_MASK_POINTS ({MASKED_POINT_BUDGET})"
                    )
                else:
                    print(f"{prefix} filter-aware masked HNSW queries", flush=True)
                    masked, masked_latency, masked_plan = (
                        execute_pgcontext_masked_queries(dsn, queries, warmup)
                    )
                    masked_recall = mean_recall_at_k(
                        filtered_exact, masked, TOP_K
                    )
                    masked_full_result_rate = statistics.fmean(
                        len(rows) == TOP_K for rows in masked
                    )
            accumulator = accumulators[system]
            accumulator["load_seconds"].append(load_seconds)
            accumulator["build_seconds"].append(build_seconds)
            if table_bytes is not None and index_bytes is not None:
                accumulator["table_bytes"].append(table_bytes)
                accumulator["index_bytes"].append(index_bytes)
            accumulator["exact_latency"].extend(exact_latency)
            accumulator["ann_latency"].extend(ann_latency)
            accumulator["ann_recall"].append(ann_recall)
            accumulator["filtered_exact_latency"].extend(filtered_exact_latency)
            accumulator["filtered_ann_latency"].extend(filtered_ann_latency)
            accumulator["filtered_ann_recall"].append(filtered_ann_recall)
            accumulator["filtered_full_result_rate"].append(full_result_rate)
            accumulator["masked_latency"].extend(masked_latency)
            accumulator["masked_recall"].append(masked_recall)
            accumulator["masked_full_result_rate"].append(masked_full_result_rate)
            accumulator["ann_plan"] = ann_plan
            accumulator["filtered_plan"] = filtered_plan
            accumulator["masked_plan"] = masked_plan
            trial_report["systems"][system] = {
                "load_seconds": load_seconds,
                "hnsw_build_seconds": build_seconds,
                "exact": summarize_latency(exact_latency),
                "ann": {
                    **summarize_latency(ann_latency),
                    "recall_at_10": ann_recall,
                },
                "filtered_ann_10_percent": {
                    **summarize_latency(filtered_ann_latency),
                    "recall_at_10": filtered_ann_recall,
                    "full_result_rate": full_result_rate,
                },
            }
            if system == "pgcontext" and masked_latency:
                trial_report["systems"][system]["filter_aware_masked_10_percent"] = {
                    **summarize_latency(masked_latency),
                    "recall_at_10": masked_recall,
                    "full_result_rate": masked_full_result_rate,
                }
        report["trials"].append(trial_report)
        (output_dir / "results.partial.json").write_text(
            json.dumps(report, indent=2) + "\n"
        )

    for system, accumulator in accumulators.items():
        load_trials = accumulator["load_seconds"]
        build_trials = accumulator["build_seconds"]
        version = {"pgcontext": "0.1.0", "pgvector": "0.8.5", "qdrant": "1.18.2"}[system]
        system_report = {
            "extension_version": version,
            "load_seconds": statistics.median(load_trials),
            "load_seconds_trials": load_trials,
            "hnsw_build_seconds": statistics.median(build_trials),
            "hnsw_build_seconds_trials": build_trials,
            "exact": summarize_latency(accumulator["exact_latency"]),
            "ann": {
                **summarize_latency(accumulator["ann_latency"]),
                "recall_at_10": statistics.fmean(accumulator["ann_recall"]),
                "plan": accumulator["ann_plan"],
            },
            "filtered_exact_10_percent": summarize_latency(
                accumulator["filtered_exact_latency"]
            ),
            "filtered_ann_10_percent": {
                **summarize_latency(accumulator["filtered_ann_latency"]),
                "recall_at_10": statistics.fmean(
                    accumulator["filtered_ann_recall"]
                ),
                "full_result_rate": statistics.fmean(
                    accumulator["filtered_full_result_rate"]
                ),
                "plan": accumulator["filtered_plan"],
            },
        }
        if accumulator["table_bytes"]:
            system_report["table_bytes"] = int(
                statistics.median(accumulator["table_bytes"])
            )
            system_report["hnsw_index_bytes"] = int(
                statistics.median(accumulator["index_bytes"])
            )
        if system == "pgcontext":
            if accumulator["masked_latency"]:
                system_report["filter_aware_masked_10_percent"] = {
                    **summarize_latency(accumulator["masked_latency"]),
                    "recall_at_10": statistics.fmean(accumulator["masked_recall"]),
                    "full_result_rate": statistics.fmean(
                        accumulator["masked_full_result_rate"]
                    ),
                    "plan": accumulator["masked_plan"],
                }
            else:
                system_report["filter_aware_masked_10_percent"] = {
                    "skipped": accumulator["masked_plan"],
                }
        report["systems"][system] = system_report
    results_path = output_dir / "results.json"
    results_path.write_text(json.dumps(report, indent=2) + "\n")
    return report


def prepare_synthetic_data(output_dir: Path, corpus_rows: int) -> dict[str, object]:
    """Generate a seeded clustered corpus for scale lanes beyond SciFact."""
    output_dir.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(SEED)
    centers = rng.normal(size=(SYNTHETIC_CLUSTERS, MODEL_DIMENSIONS))
    assignments = rng.integers(0, SYNTHETIC_CLUSTERS, corpus_rows)
    corpus = centers[assignments] + 0.35 * rng.normal(
        size=(corpus_rows, MODEL_DIMENSIONS)
    )
    corpus /= np.linalg.norm(corpus, axis=1, keepdims=True)
    query_sources = rng.choice(corpus_rows, SYNTHETIC_QUERY_ROWS, replace=False)
    queries = corpus[query_sources] + 0.15 * rng.normal(
        size=(SYNTHETIC_QUERY_ROWS, MODEL_DIMENSIONS)
    )
    queries /= np.linalg.norm(queries, axis=1, keepdims=True)
    np.save(output_dir / "corpus.npy", corpus.astype(np.float32))
    np.save(output_dir / "queries.npy", queries.astype(np.float32))
    metadata = {
        "dataset": f"synthetic-gaussian-{SYNTHETIC_CLUSTERS}-clusters",
        "corpus_rows": corpus_rows,
        "query_rows": SYNTHETIC_QUERY_ROWS,
        "model": "none (seeded synthetic)",
        "dimensions": MODEL_DIMENSIONS,
        "seed": SEED,
    }
    (output_dir / "dataset.json").write_text(json.dumps(metadata, indent=2) + "\n")
    return metadata


def run_sweep(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
    ef_values: Sequence[int],
) -> dict[str, object]:
    """Sweep ef_search per system and emit latency-vs-recall Pareto curves."""
    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    queries = [all_queries[index] for index in indexes]
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "corpus_rows": len(corpus),
            "queries": sample_count,
            "warmup": warmup,
            "top_k": TOP_K,
            "seed": SEED,
            "hnsw_m": HNSW_M,
            "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
            "pgcontext_build_parallel_workers": PGCONTEXT_BUILD_WORKERS or "default",
            "systems_measured": list(BENCH_SYSTEMS),
            "ef_values": list(ef_values),
        },
        "curves": {},
    }
    for system in BENCH_SYSTEMS:
        dsn, _, _, _, _ = load_system(admin_dsn, system, corpus)
        exact, _, _ = execute_queries(dsn, system, queries, False, None, warmup)
        curve = []
        for ef in ef_values:
            ann, latencies, _ = execute_queries(
                dsn, system, queries, True, None, warmup, ef_search=ef
            )
            point = {
                "ef_search": ef,
                **summarize_latency(latencies),
                "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
            }
            curve.append(point)
            print(
                f"[sweep] [{system}] ef={ef} p50={point['p50_ms']:.4f}ms "
                f"recall={point['recall_at_10']:.4f}",
                flush=True,
            )
        report["curves"][system] = curve
    if not BENCH_QDRANT:
        (output_dir / "sweep.json").write_text(json.dumps(report, indent=2) + "\n")
        return report
    client, _, _ = load_qdrant(corpus)
    exact, _, _ = execute_qdrant_queries(client, queries, False, None, warmup)
    curve = []
    for ef in ef_values:
        ann, latencies, _ = execute_qdrant_queries(
            client, queries, True, None, warmup, ef_search=ef
        )
        point = {
            "ef_search": ef,
            **summarize_latency(latencies),
            "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
        }
        curve.append(point)
        print(
            f"[sweep] [qdrant] ef={ef} p50={point['p50_ms']:.4f}ms "
            f"recall={point['recall_at_10']:.4f}",
            flush=True,
        )
    report["qdrant_effort"] = qdrant_effort_metadata(client, ef_values)
    print(
        f"[sweep] [qdrant] segments={report['qdrant_effort'].get('segments_count')} "
        "(hnsw_ef applies per segment)",
        flush=True,
    )
    client.close()
    report["curves"]["qdrant"] = curve
    (output_dir / "sweep.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def run_filtered_sweep(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
) -> dict[str, object]:
    """Measure filtered ANN across selectivity lanes for all systems."""
    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    queries = [all_queries[index] for index in indexes]
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "corpus_rows": len(corpus),
            "queries": sample_count,
            "warmup": warmup,
            "top_k": TOP_K,
            "seed": SEED,
            "hnsw_m": HNSW_M,
            "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
            "pgcontext_build_parallel_workers": PGCONTEXT_BUILD_WORKERS or "default",
            "systems_measured": list(BENCH_SYSTEMS),
            "pgcontext_hnsw_ef_search": PGCONTEXT_HNSW_EF_SEARCH,
            "pgvector_hnsw_ef_search": PGVECTOR_HNSW_EF_SEARCH,
            "qdrant_hnsw_ef_search": QDRANT_HNSW_EF_SEARCH,
            "selectivity_lanes": {
                label: {"column": column, "modulo": modulo}
                for label, (column, modulo) in SELECTIVITY_LANES.items()
            },
        },
        "lanes": {},
    }
    pg_state = {}
    for system in BENCH_SYSTEMS:
        dsn, _, _, _, _ = load_system(admin_dsn, system, corpus)
        pg_state[system] = dsn
    setup_pgcontext_collection(pg_state["pgcontext"])
    client, _, _ = load_qdrant(corpus)
    for label, (column, _modulo) in SELECTIVITY_LANES.items():
        lane: dict[str, object] = {}
        for system in BENCH_SYSTEMS:
            dsn = pg_state[system]
            exact, exact_latency, _ = execute_queries(
                dsn, system, queries, False, column, warmup
            )
            ann, ann_latency, _ = execute_queries(
                dsn, system, queries, True, column, warmup
            )
            entry = {
                "filtered_exact": summarize_latency(exact_latency),
                "filtered_ann": {
                    **summarize_latency(ann_latency),
                    "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
                    "full_result_rate": statistics.fmean(
                        len(rows) == TOP_K for rows in ann
                    ),
                },
            }
            if system == "pgcontext":
                if len(corpus) // FILTER_MODULO[column] <= MASKED_POINT_BUDGET:
                    masked, masked_latency, _ = execute_pgcontext_masked_queries(
                        dsn, queries, warmup, filter_column=column
                    )
                    entry["filter_aware_masked"] = {
                        **summarize_latency(masked_latency),
                        "recall_at_10": mean_recall_at_k(exact, masked, TOP_K),
                        "full_result_rate": statistics.fmean(
                            len(rows) == TOP_K for rows in masked
                        ),
                    }
                else:
                    entry["filter_aware_masked"] = {
                        "skipped": "mask exceeds MAX_HNSW_CANDIDATE_MASK_POINTS"
                    }
                adaptive, adaptive_latency = execute_pgcontext_collection_queries(
                    dsn, queries, column, warmup
                )
                entry["collection_adaptive"] = {
                    **summarize_latency(adaptive_latency),
                    "recall_at_10": mean_recall_at_k(exact, adaptive, TOP_K),
                    "full_result_rate": statistics.fmean(
                        len(rows) == TOP_K for rows in adaptive
                    ),
                }
            lane[system] = entry
            print(f"[filtered-sweep] [{label}] [{system}] done", flush=True)
        exact, exact_latency, _ = execute_qdrant_queries(
            client, queries, False, column, warmup
        )
        ann, ann_latency, _ = execute_qdrant_queries(
            client, queries, True, column, warmup
        )
        lane["qdrant"] = {
            "filtered_exact": summarize_latency(exact_latency),
            "filtered_ann": {
                **summarize_latency(ann_latency),
                "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
                "full_result_rate": statistics.fmean(
                    len(rows) == TOP_K for rows in ann
                ),
            },
        }
        print(f"[filtered-sweep] [{label}] [qdrant] done", flush=True)
        report["lanes"][label] = lane
    client.close()
    (output_dir / "filtered-sweep.json").write_text(
        json.dumps(report, indent=2) + "\n"
    )
    return report


def _pg_concurrency_worker(
    payload: tuple[str, str, list[str], int, int],
) -> tuple[list[float], int]:
    """Run the query list on one connection; return latencies and backend RSS KiB."""
    dsn, system, serialized_queries, warmup, ef = payload
    sql = query_sql(system, True, None)
    latencies_ms: list[float] = []
    with psycopg.connect(dsn, autocommit=True) as connection:
        connection.execute("SET jit = off")
        connection.execute("SET max_parallel_workers_per_gather = 0")
        connection.execute("SET enable_seqscan = off")
        if system == "pgcontext":
            connection.execute(f"SET pgcontext.hnsw_ef_search = {ef}")
        else:
            connection.execute(f"SET hnsw.ef_search = {ef}")
        backend_pid = connection.execute("SELECT pg_backend_pid()").fetchone()[0]
        for literal in serialized_queries[:warmup]:
            connection.execute(sql, (literal,)).fetchall()
        for literal in serialized_queries:
            started = time.perf_counter_ns()
            connection.execute(sql, (literal,)).fetchall()
            latencies_ms.append((time.perf_counter_ns() - started) / 1_000_000)
        rss_kb = int(
            subprocess.run(
                ["ps", "-o", "rss=", "-p", str(backend_pid)],
                check=False,
                capture_output=True,
                text=True,
            ).stdout.strip()
            or 0
        )
    return latencies_ms, rss_kb


def run_concurrency(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
    worker_counts: Sequence[int],
) -> dict[str, object]:
    """Measure PostgreSQL-system throughput and backend memory under N clients."""
    import multiprocessing

    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    serialized = [vector_literal(all_queries[index]) for index in indexes]
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "corpus_rows": len(corpus),
            "queries_per_worker": sample_count,
            "warmup": warmup,
            "top_k": TOP_K,
            "seed": SEED,
            "hnsw_m": HNSW_M,
            "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
            "pgcontext_build_parallel_workers": PGCONTEXT_BUILD_WORKERS or "default",
            "systems_measured": list(BENCH_SYSTEMS),
            "pgcontext_hnsw_ef_search": PGCONTEXT_HNSW_EF_SEARCH,
            "pgvector_hnsw_ef_search": PGVECTOR_HNSW_EF_SEARCH,
            "worker_counts": list(worker_counts),
        },
        "systems": {},
    }
    context = multiprocessing.get_context("spawn")
    for system in BENCH_SYSTEMS:
        dsn, _, _, _, _ = load_system(admin_dsn, system, corpus)
        ef = (
            PGCONTEXT_HNSW_EF_SEARCH
            if system == "pgcontext"
            else PGVECTOR_HNSW_EF_SEARCH
        )
        levels = {}
        for workers in worker_counts:
            payloads = [(dsn, system, serialized, warmup, ef)] * workers
            started = time.perf_counter()
            with context.Pool(processes=workers) as pool:
                outcomes = pool.map(_pg_concurrency_worker, payloads)
            wall_seconds = time.perf_counter() - started
            latencies = [value for latencies, _ in outcomes for value in latencies]
            rss_total_kb = sum(rss for _, rss in outcomes)
            levels[str(workers)] = {
                **summarize_latency(latencies),
                "wall_seconds": wall_seconds,
                "aggregate_qps": (workers * sample_count) / wall_seconds,
                "backend_rss_total_kb": rss_total_kb,
            }
            print(
                f"[concurrency] [{system}] workers={workers} "
                f"qps={levels[str(workers)]['aggregate_qps']:.0f} "
                f"rss_total={rss_total_kb / 1024:.0f}MiB",
                flush=True,
            )
        report["systems"][system] = levels
    (output_dir / "concurrency.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def run_churn(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
    rounds: int,
    churn_percent: float,
) -> dict[str, object]:
    """Measure PostgreSQL-system latency/recall stability under update churn.

    Each round rewrites a seeded sample of vectors in place, VACUUMs, then
    re-measures the exact oracle and ANN lanes. The first post-churn ANN query
    is recorded separately because pgContext repacks its backend-local graph
    generation after an invalidation.
    """
    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    queries = [all_queries[index] for index in indexes]
    vector_rng = np.random.default_rng(SEED + 1)
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "corpus_rows": len(corpus),
            "queries": sample_count,
            "warmup": warmup,
            "rounds": rounds,
            "churn_percent": churn_percent,
            "top_k": TOP_K,
            "seed": SEED,
            "hnsw_m": HNSW_M,
            "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
            "pgcontext_build_parallel_workers": PGCONTEXT_BUILD_WORKERS or "default",
            "systems_measured": list(BENCH_SYSTEMS),
            "pgcontext_hnsw_ef_search": PGCONTEXT_HNSW_EF_SEARCH,
            "pgvector_hnsw_ef_search": PGVECTOR_HNSW_EF_SEARCH,
        },
        "systems": {},
    }
    churn_rows = max(1, int(len(corpus) * churn_percent / 100))
    for system in BENCH_SYSTEMS:
        dsn, _, _, _, _ = load_system(admin_dsn, system, corpus)
        rounds_report = []
        exact, _, _ = execute_queries(dsn, system, queries, False, None, warmup)
        ann, ann_latency, _ = execute_queries(dsn, system, queries, True, None, warmup)
        baseline = {
            "round": 0,
            "ann": {
                **summarize_latency(ann_latency),
                "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
            },
        }
        rounds_report.append(baseline)
        print(
            f"[churn] [{system}] round=0 p50={baseline['ann']['p50_ms']:.3f}ms "
            f"recall={baseline['ann']['recall_at_10']:.4f}",
            flush=True,
        )
        with psycopg.connect(dsn, autocommit=True) as connection:
            for round_index in range(1, rounds + 1):
                target_ids = rng.sample(range(1, len(corpus) + 1), churn_rows)
                replacements = vector_rng.normal(size=(churn_rows, MODEL_DIMENSIONS))
                replacements /= np.linalg.norm(replacements, axis=1, keepdims=True)
                connection.execute(
                    "CREATE TEMP TABLE IF NOT EXISTS churn_batch "
                    "(id bigint, embedding text)"
                )
                connection.execute("TRUNCATE churn_batch")
                with connection.cursor().copy(
                    "COPY churn_batch (id, embedding) FROM STDIN"
                ) as copy:
                    for row_id, vector in zip(target_ids, replacements, strict=True):
                        copy.write_row((row_id, vector_literal(vector)))
                update_started = time.perf_counter()
                connection.execute(
                    "UPDATE items SET embedding = churn_batch.embedding::vector "
                    "FROM churn_batch WHERE items.id = churn_batch.id"
                )
                update_seconds = time.perf_counter() - update_started
                connection.execute("VACUUM (ANALYZE) items")
                index_bytes = connection.execute(
                    "SELECT pg_relation_size('items_embedding_hnsw')"
                ).fetchone()[0]
                exact, _, _ = execute_queries(
                    dsn, system, queries, False, None, warmup
                )
                # Measure the first post-churn ANN query on a fresh connection
                # with no warmup: this exposes any per-backend re-preparation
                # (for pgContext, the packed-generation rebuild).
                first, first_latency, _ = execute_queries(
                    dsn, system, queries[:1], True, None, 0
                )
                ann, ann_latency, _ = execute_queries(
                    dsn, system, queries, True, None, warmup
                )
                entry = {
                    "round": round_index,
                    "updated_rows": churn_rows,
                    "update_seconds": update_seconds,
                    "updates_per_second": churn_rows / update_seconds,
                    "index_bytes": index_bytes,
                    "first_query_ms": first_latency[0],
                    "ann": {
                        **summarize_latency(ann_latency),
                        "recall_at_10": mean_recall_at_k(exact, ann, TOP_K),
                        "full_result_rate": statistics.fmean(
                            len(rows) == TOP_K for rows in ann
                        ),
                    },
                }
                rounds_report.append(entry)
                print(
                    f"[churn] [{system}] round={round_index} "
                    f"p50={entry['ann']['p50_ms']:.3f}ms "
                    f"recall={entry['ann']['recall_at_10']:.4f} "
                    f"first={entry['first_query_ms']:.3f}ms "
                    f"update={update_seconds:.1f}s "
                    f"({entry['updates_per_second']:.0f} rows/s) "
                    f"index={index_bytes / 2**20:.1f}MiB",
                    flush=True,
                )
        report["systems"][system] = {"rounds": rounds_report}
    (output_dir / "churn.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def run_cold_cache(
    output_dir: Path,
    admin_dsn: str,
    sample_count: int,
    warmup: int,
    restart_command: str,
) -> dict[str, object]:
    """Measure first-query latency after a full PostgreSQL restart.

    The caller supplies the restart command (for example
    `pg_ctl -D /opt/homebrew/var/postgresql@17 restart -w`); the lane loads
    and builds each system, restarts the server, then records each of the
    first `warmup` queries individually before measuring steady state.
    """
    corpus = np.load(output_dir / "corpus.npy", mmap_mode="r")
    all_queries = np.load(output_dir / "queries.npy", mmap_mode="r")
    rng = random.Random(SEED)
    indexes = rng.sample(range(len(all_queries)), sample_count)
    queries = [all_queries[index] for index in indexes]
    report: dict[str, object] = {
        "metadata": system_metadata(admin_dsn),
        "configuration": {
            "corpus_rows": len(corpus),
            "queries": sample_count,
            "warmup": warmup,
            "top_k": TOP_K,
            "seed": SEED,
            "restart_command": restart_command,
            "pgcontext_hnsw_ef_search": PGCONTEXT_HNSW_EF_SEARCH,
            "pgvector_hnsw_ef_search": PGVECTOR_HNSW_EF_SEARCH,
        },
        "systems": {},
    }
    state = {
        system: load_system(admin_dsn, system, corpus)[0]
        for system in BENCH_SYSTEMS
    }
    subprocess.run(restart_command, shell=True, check=True)
    deadline = time.monotonic() + 120
    while True:
        try:
            with psycopg.connect(admin_dsn, connect_timeout=3):
                break
        except psycopg.OperationalError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.5)
    for system in BENCH_SYSTEMS:
        dsn = state[system]
        cold_first: list[float] = []
        for query in queries[: max(warmup, 1)]:
            _, latency, _ = execute_queries(dsn, system, [query], True, None, 0)
            cold_first.append(latency[0])
        _, steady_latency, _ = execute_queries(
            dsn, system, queries, True, None, warmup
        )
        entry = {
            "cold_first_queries_ms": cold_first,
            "cold_first_ms": cold_first[0],
            "cold_p50_ms": percentile(cold_first, 50),
            "steady": summarize_latency(steady_latency),
        }
        report["systems"][system] = entry
        print(
            f"[cold] [{system}] first={entry['cold_first_ms']:.3f}ms "
            f"cold_p50={entry['cold_p50_ms']:.3f}ms "
            f"steady_p50={entry['steady']['p50_ms']:.3f}ms",
            flush=True,
        )
    (output_dir / "cold-cache.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=(
            "prepare",
            "run",
            "sweep",
            "filtered-sweep",
            "concurrency",
            "churn",
            "cold-cache",
            "all",
        ),
        nargs="?",
        default="all",
    )
    parser.add_argument(
        "--output-dir", type=Path, default=Path("target/pgvector-comparison")
    )
    parser.add_argument(
        "--dsn",
        default=os.environ.get(
            "PGCONTEXT_BENCH_DSN", "host=localhost port=28817 dbname=postgres"
        ),
    )
    parser.add_argument("--queries", type=int, default=DEFAULT_QUERIES)
    parser.add_argument("--warmup", type=int, default=DEFAULT_WARMUP)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument(
        "--synthetic",
        type=int,
        default=0,
        help="prepare a seeded synthetic corpus with this many rows instead of SciFact",
    )
    parser.add_argument(
        "--ef-values",
        default=",".join(str(value) for value in DEFAULT_SWEEP_EF_VALUES),
        help="comma-separated ef_search values for the sweep command",
    )
    parser.add_argument(
        "--workers",
        default="1,8,32",
        help="comma-separated client counts for the concurrency command",
    )
    parser.add_argument(
        "--rounds",
        type=int,
        default=5,
        help="churn rounds for the churn command",
    )
    parser.add_argument(
        "--churn-percent",
        type=float,
        default=5.0,
        help="percent of corpus rewritten per churn round",
    )
    parser.add_argument(
        "--restart-cmd",
        default=os.environ.get("PGCONTEXT_BENCH_RESTART_CMD", ""),
        help="shell command that restarts PostgreSQL for the cold-cache command",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.queries <= 0 or args.warmup < 0 or args.trials <= 0:
        raise SystemExit(
            "queries and trials must be positive and warmup must be non-negative"
        )
    if args.command in ("prepare", "all"):
        if args.synthetic > 0:
            print(json.dumps(prepare_synthetic_data(args.output_dir, args.synthetic), indent=2))
        else:
            print(json.dumps(prepare_data(args.output_dir), indent=2))
    if args.command in ("run", "all"):
        report = run_benchmark(
            args.output_dir, args.dsn, args.queries, args.warmup, args.trials
        )
        print(json.dumps(report, indent=2))
    if args.command == "sweep":
        ef_values = tuple(int(value) for value in args.ef_values.split(","))
        run_sweep(args.output_dir, args.dsn, args.queries, args.warmup, ef_values)
    if args.command == "filtered-sweep":
        run_filtered_sweep(args.output_dir, args.dsn, args.queries, args.warmup)
    if args.command == "concurrency":
        worker_counts = tuple(int(value) for value in args.workers.split(","))
        run_concurrency(
            args.output_dir, args.dsn, args.queries, args.warmup, worker_counts
        )
    if args.command == "churn":
        run_churn(
            args.output_dir,
            args.dsn,
            args.queries,
            args.warmup,
            args.rounds,
            args.churn_percent,
        )
    if args.command == "cold-cache":
        if not args.restart_cmd:
            raise SystemExit(
                "cold-cache requires --restart-cmd or PGCONTEXT_BENCH_RESTART_CMD"
            )
        run_cold_cache(
            args.output_dir, args.dsn, args.queries, args.warmup, args.restart_cmd
        )


if __name__ == "__main__":
    main()
