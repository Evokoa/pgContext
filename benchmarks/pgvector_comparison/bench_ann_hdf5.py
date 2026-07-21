#!/usr/bin/env python3
"""Benchmark against a recognized ann-benchmarks HDF5 dataset.

Unlike the SciFact/synthetic harness in benchmark.py, this measures all three
systems on a standard public dataset (e.g. glove-100-angular) using the
dataset's own precomputed ground-truth neighbors -- the setup an outside
reviewer expects. It is intentionally standalone and dimension/metric-driven
so the 384-dim assumptions in benchmark.py do not leak in.

Fairness notes baked in:
  * recall is measured against the HDF5 `neighbors` ground truth, not against
    each system's own exact scan;
  * every system is queried at the SAME hnsw_ef in a sweep;
  * intended to run all three systems in matched Docker deployment (the DSN and
    Qdrant URL are passed in), so the client/transport boundary is symmetric.

HDF5 layout (ann-benchmarks): train (N,D), test (Q,D), neighbors (Q,100 int,
0-based indices into train), attrs['distance'] in {'angular','euclidean'}.
Row i of train is loaded with id=i so returned ids compare directly to
`neighbors`.
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from pathlib import Path

import h5py
import numpy as np
import psycopg
from qdrant_client import QdrantClient, models

TOP_K = 10
HNSW_M = 16
HNSW_EF_CONSTRUCTION = int(os.environ.get("PGCONTEXT_BENCH_EF_CONSTRUCTION", "64"))
# Same parallel-build budget for both PostgreSQL engines, so build time is a
# fair comparison. pgContext defaults to 1 (serial) and pgvector to 2, which
# would make pgContext look artificially slow. pgContext uses in-process
# threads; pgvector uses parallel maintenance workers (leader + N-1).
BUILD_WORKERS = int(os.environ.get("PGCONTEXT_BENCH_BUILD_WORKERS", "8"))
MAINTENANCE_WORK_MEM = os.environ.get("PGCONTEXT_BENCH_MAINTENANCE_WORK_MEM", "4GB")
QDRANT_COLLECTION = "ann_bench"

# angular in ann-benchmarks == cosine. Each value picks the matching operator
# class / distance for the three systems.
METRIC = {
    "angular": {
        "pgcontext_ops": "pgcontext.vector_hnsw_cosine_ops",
        "pgcontext_op": "OPERATOR(pgcontext.<=>)",
        "pgvector_ops": "vector_cosine_ops",
        "pgvector_op": "<=>",
        "qdrant": models.Distance.COSINE,
    },
    "euclidean": {
        "pgcontext_ops": "pgcontext.vector_hnsw_ops",
        "pgcontext_op": "OPERATOR(pgcontext.<->)",
        "pgvector_ops": "vector_l2_ops",
        "pgvector_op": "<->",
        "qdrant": models.Distance.EUCLID,
    },
}


def percentile(values, pct):
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = (pct / 100) * (len(ordered) - 1)
    low = int(rank)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def vector_literal(vector) -> str:
    return "[" + ",".join(f"{float(x):.8f}" for x in vector) + "]"


def recall_at_k(returned_ids, truth_ids, k) -> float:
    if not truth_ids:
        return 0.0
    hits = len(set(returned_ids[:k]) & set(truth_ids[:k]))
    return hits / min(k, len(truth_ids))


def load_dataset(path: Path, max_rows: int, max_queries: int):
    with h5py.File(path, "r") as f:
        metric = str(f.attrs["distance"])
        train = np.asarray(f["train"], dtype=np.float32)
        test = np.asarray(f["test"], dtype=np.float32)
        neighbors = np.asarray(f["neighbors"], dtype=np.int64)
    if max_rows and max_rows < len(train):
        # Smoke mode: subset the corpus AND recompute ground truth on the
        # subset, since the shipped neighbors index the full train set. Only
        # used for plumbing checks; real runs pass the whole dataset.
        train = train[:max_rows]
        test = test[:max_queries] if max_queries else test
        neighbors = _exact_neighbors(train, test, metric)
    elif max_queries and max_queries < len(test):
        test = test[:max_queries]
        neighbors = neighbors[:max_queries]
    return metric, train, test, neighbors


def _exact_neighbors(train, test, metric):
    truth = np.empty((len(test), TOP_K), dtype=np.int64)
    if metric == "angular":
        tn = train / np.linalg.norm(train, axis=1, keepdims=True)
        qn = test / np.linalg.norm(test, axis=1, keepdims=True)
        for i, q in enumerate(qn):
            truth[i] = np.argsort(-(tn @ q))[:TOP_K]
    else:
        for i, q in enumerate(test):
            truth[i] = np.argsort(np.linalg.norm(train - q, axis=1))[:TOP_K]
    return truth


# --------------------------------------------------------------------------- #
# Postgres systems (pgcontext, pgvector) -- same engine, different opclass.
# --------------------------------------------------------------------------- #
def pg_load(dsn, system, train, dim, metric):
    ops = METRIC[metric][f"{system}_ops"]
    with psycopg.connect(dsn, autocommit=True) as conn:
        conn.execute("DROP TABLE IF EXISTS items")
        conn.execute(f"CREATE TABLE items (id bigint PRIMARY KEY, embedding vector({dim}))")
        started = time.perf_counter()
        with conn.cursor().copy("COPY items (id, embedding) FROM STDIN") as copy:
            for i, vector in enumerate(train):
                copy.write_row((i, vector_literal(vector)))
        load_s = time.perf_counter() - started
        conn.execute("ANALYZE items")
        conn.execute(f"SET maintenance_work_mem = '{MAINTENANCE_WORK_MEM}'")
        if system == "pgcontext":
            conn.execute(f"SET pgcontext.hnsw_ef_construction = {HNSW_EF_CONSTRUCTION}")
            conn.execute(f"SET pgcontext.hnsw_build_parallel_workers = {BUILD_WORKERS}")
            create = f"CREATE INDEX items_hnsw ON items USING pgcontext_hnsw (embedding {ops})"
        else:
            conn.execute(f"SET max_parallel_maintenance_workers = {max(1, BUILD_WORKERS - 1)}")
            create = (
                f"CREATE INDEX items_hnsw ON items USING hnsw (embedding {ops}) "
                f"WITH (m={HNSW_M}, ef_construction={HNSW_EF_CONSTRUCTION})"
            )
        started = time.perf_counter()
        conn.execute(create)
        build_s = time.perf_counter() - started
    return load_s, build_s


def pg_query(dsn, system, test, neighbors, ef, metric):
    op = METRIC[metric][f"{system}_op"]
    sql = f"SELECT id FROM items ORDER BY embedding {op} %s::vector LIMIT {TOP_K}"
    literals = [(vector_literal(v),) for v in test]
    recalls, latencies = [], []
    with psycopg.connect(dsn, autocommit=True) as conn:
        conn.execute("SET jit = off")
        conn.execute("SET max_parallel_workers_per_gather = 0")
        conn.execute("SET enable_seqscan = off")
        if system == "pgcontext":
            conn.execute(f"SET pgcontext.hnsw_ef_search = {ef}")
        else:
            conn.execute(f"SET hnsw.ef_search = {ef}")
        plan = "\n".join(r[0] for r in conn.execute("EXPLAIN (COSTS OFF) " + sql, literals[0]).fetchall())
        if "items_hnsw" not in plan:
            raise RuntimeError(f"{system} did not use the HNSW index:\n{plan}")
        for i, params in enumerate(literals):
            started = time.perf_counter_ns()
            rows = conn.execute(sql, params).fetchall()
            latencies.append((time.perf_counter_ns() - started) / 1_000_000)
            recalls.append(recall_at_k([int(r[0]) for r in rows], list(neighbors[i]), TOP_K))
    return recalls, latencies


# --------------------------------------------------------------------------- #
# Qdrant
# --------------------------------------------------------------------------- #
def qdrant_load(url, grpc_port, train, dim, metric):
    client = QdrantClient(url=url, grpc_port=grpc_port, prefer_grpc=True, timeout=600)
    if client.collection_exists(QDRANT_COLLECTION):
        client.delete_collection(QDRANT_COLLECTION)
    client.create_collection(
        collection_name=QDRANT_COLLECTION,
        vectors_config=models.VectorParams(
            size=dim, distance=METRIC[metric]["qdrant"],
            hnsw_config=models.HnswConfigDiff(m=HNSW_M, ef_construct=HNSW_EF_CONSTRUCTION),
        ),
        optimizers_config=models.OptimizersConfigDiff(indexing_threshold=0),
    )
    started = time.perf_counter()
    client.upload_collection(
        collection_name=QDRANT_COLLECTION,
        vectors=train, ids=list(range(len(train))),
        batch_size=1000, parallel=1, wait=True,
    )
    load_s = time.perf_counter() - started
    started = time.perf_counter()
    client.update_collection(
        collection_name=QDRANT_COLLECTION,
        optimizers_config=models.OptimizersConfigDiff(indexing_threshold=1),
    )
    deadline = time.monotonic() + 1800
    while True:
        info = client.get_collection(QDRANT_COLLECTION)
        if str(info.status).lower().endswith("green") and int(info.indexed_vectors_count or 0) >= len(train):
            break
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Qdrant index did not finish: status={info.status} indexed={info.indexed_vectors_count}")
        time.sleep(0.2)
    build_s = time.perf_counter() - started
    segments = int(info.segments_count or 0)
    return client, load_s, build_s, segments


def qdrant_query(client, test, neighbors, ef):
    params = models.SearchParams(hnsw_ef=ef, exact=False)
    recalls, latencies = [], []
    for i, vector in enumerate(test):
        started = time.perf_counter_ns()
        resp = client.query_points(
            collection_name=QDRANT_COLLECTION, query=vector.tolist(),
            search_params=params, limit=TOP_K, with_payload=False, with_vectors=False,
        )
        latencies.append((time.perf_counter_ns() - started) / 1_000_000)
        recalls.append(recall_at_k([int(p.id) for p in resp.points], list(neighbors[i]), TOP_K))
    return recalls, latencies


def summarize(recalls, latencies):
    return {
        "recall_at_10": statistics.fmean(recalls),
        "p50_ms": percentile(latencies, 50),
        "p95_ms": percentile(latencies, 95),
        "mean_ms": statistics.fmean(latencies),
        "qps": 1000.0 / statistics.fmean(latencies) if latencies else 0.0,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hdf5", required=True, type=Path)
    ap.add_argument("--pg-dsn", default=os.environ.get("BENCH_PG_DSN", "host=localhost port=5433 dbname=postgres user=postgres password=postgres"))
    ap.add_argument("--qdrant-url", default=os.environ.get("BENCH_QDRANT_URL", "http://localhost:6333"))
    ap.add_argument("--qdrant-grpc", type=int, default=int(os.environ.get("BENCH_QDRANT_GRPC", "6334")))
    ap.add_argument("--systems", default="pgcontext,pgvector,qdrant")
    ap.add_argument("--ef-values", default="64,128,256,512")
    ap.add_argument("--max-rows", type=int, default=0, help="subset corpus (smoke only)")
    ap.add_argument("--max-queries", type=int, default=0)
    ap.add_argument("--output", type=Path, required=True)
    args = ap.parse_args()

    systems = [s.strip() for s in args.systems.split(",") if s.strip()]
    ef_values = [int(e) for e in args.ef_values.split(",")]
    metric, train, test, neighbors = load_dataset(args.hdf5, args.max_rows, args.max_queries)
    dim = train.shape[1]
    print(f"dataset={args.hdf5.name} metric={metric} train={len(train)} test={len(test)} dim={dim}", flush=True)

    report = {
        "dataset": args.hdf5.name, "metric": metric,
        "corpus_rows": len(train), "queries": len(test), "dimensions": dim,
        "top_k": TOP_K, "hnsw_m": HNSW_M, "hnsw_ef_construction": HNSW_EF_CONSTRUCTION,
        "build_parallel_workers": BUILD_WORKERS,
        "ef_values": ef_values, "systems_measured": systems,
        "deployment": "all systems in Docker (matched transport boundary)",
        "recall_ground_truth": "dataset-provided neighbors" if not args.max_rows else "recomputed exact on subset (smoke)",
        "results": {},
    }

    def persist():
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")

    for system in systems:
        print(f"[{system}] loading + building", flush=True)
        try:
            if system == "qdrant":
                client, load_s, build_s, segments = qdrant_load(args.qdrant_url, args.qdrant_grpc, train, dim, metric)
                curve = []
                for ef in ef_values:
                    recalls, latencies = qdrant_query(client, test, neighbors, ef)
                    point = {"ef_search": ef, **summarize(recalls, latencies)}
                    curve.append(point)
                    print(f"[qdrant] ef={ef} recall={point['recall_at_10']:.4f} p50={point['p50_ms']:.3f}ms", flush=True)
                client.close()
                report["results"][system] = {"load_s": load_s, "build_s": build_s, "segments_count": segments,
                                             "effective_candidates": {str(e): e * max(1, segments) for e in ef_values}, "curve": curve}
                persist()
            else:
                load_s, build_s = pg_load(args.pg_dsn, system, train, dim, metric)
                curve = []
                for ef in ef_values:
                    recalls, latencies = pg_query(args.pg_dsn, system, test, neighbors, ef, metric)
                    point = {"ef_search": ef, **summarize(recalls, latencies)}
                    curve.append(point)
                    print(f"[{system}] ef={ef} recall={point['recall_at_10']:.4f} p50={point['p50_ms']:.3f}ms", flush=True)
                report["results"][system] = {"load_s": load_s, "build_s": build_s, "curve": curve}
                persist()
        except Exception as error:  # noqa: BLE001 - one system's failure must not lose the others
            print(f"[{system}] FAILED: {error}", flush=True)
            report["results"][system] = {"error": str(error)}
            persist()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"wrote {args.output}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
