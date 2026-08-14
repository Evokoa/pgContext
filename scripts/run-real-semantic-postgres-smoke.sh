#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_root="${PGCONTEXT_SEMANTIC_MODEL_DIR:-${repository_root}/target/real-semantic-models}"
python="${runtime_root}/venv/bin/python"
adapter="${repository_root}/tools/real-semantic-models/semantic_models.py"
pg_major="${PG_MAJOR:-17}"
port="${PGCONTEXT_REAL_MODEL_PORT:-28917}"

case "${pg_major}" in
  17 | 18) ;;
  *)
    echo "PG_MAJOR must be 17 or 18" >&2
    exit 2
    ;;
esac
[[ "${port}" =~ ^[1-9][0-9]{0,4}$ ]] && (( port <= 65535 )) || {
  echo "PGCONTEXT_REAL_MODEL_PORT must be a valid TCP port" >&2
  exit 2
}
[[ -x "${python}" ]] || {
  echo "optional runtime is missing; run scripts/install-real-semantic-models.sh" >&2
  exit 1
}

if [[ -n "${PG_CONFIG:-}" ]]; then
  pg_config="${PG_CONFIG}"
else
  pg_config="$(cargo pgrx info pg-config "pg${pg_major}")"
fi
pg_bin="$(dirname "${pg_config}")"
initdb="${pg_bin}/initdb"
pg_ctl="${pg_bin}/pg_ctl"
psql="${pg_bin}/psql"
for executable in "${initdb}" "${pg_ctl}" "${psql}"; do
  [[ -x "${executable}" ]] || {
    echo "required PostgreSQL executable is missing: ${executable}" >&2
    exit 1
  }
done

tmp_base="${TMPDIR:-/tmp}"
cluster_root="$(mktemp -d "${tmp_base%/}/pgcontext-real-semantic-pg${pg_major}.XXXXXX")"
cluster_data="${cluster_root}/data"
cluster_log="${cluster_root}/postgres.log"
envelope="${cluster_root}/rerank-envelope.json"
response="${cluster_root}/rerank-response.json"
embedding_request="${cluster_root}/embedding-request.json"
embedding_response="${cluster_root}/embedding-response.json"
cluster_started=false
cleanup() {
  if [[ "${cluster_started}" == true ]]; then
    "${pg_ctl}" stop -D "${cluster_data}" -m fast >/dev/null
  fi
  case "${cluster_root}" in
    "${tmp_base%/}/pgcontext-real-semantic-pg${pg_major}."*) rm -rf -- "${cluster_root}" ;;
    *) echo "refusing to remove unexpected temporary path: ${cluster_root}" >&2 ;;
  esac
}
trap cleanup EXIT

LC_ALL=C "${initdb}" -D "${cluster_data}" --no-sync --auth=trust >/dev/null
"${pg_ctl}" start -D "${cluster_data}" -l "${cluster_log}" \
  -o "-F -h 127.0.0.1 -p ${port}" -w >/dev/null
cluster_started=true
psql_command=("${psql}" -X -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "${port}" -d postgres)

"${psql_command[@]}" -q >/dev/null <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.real_semantic_docs (
    id bigint PRIMARY KEY,
    body text NOT NULL,
    source_version bigint NOT NULL DEFAULT 1
);
INSERT INTO public.real_semantic_docs (id, body) VALUES
    (1, 'Bananas are yellow fruit grown in tropical climates.'),
    (2, 'Mars is known as the Red Planet because iron minerals oxidize on its surface.'),
    (3, 'Saturn is a gas giant recognized by its rings.');
SELECT pgcontext.create_collection('real_semantic_docs', 'public.real_semantic_docs');
SELECT pgcontext.backfill_points('real_semantic_docs', 100);
SELECT pgcontext.register_semantic_rerank_source(
    'real_semantic_docs', 'body', 'body', 'source_version'
);
SQL

"${psql_command[@]}" -qAt <<'SQL' >"${envelope}"
WITH candidates AS (
    SELECT pg_catalog.jsonb_agg(
               pg_catalog.jsonb_build_object(
                   'occurrence_id', points.source_key::bigint,
                   'point_id', points.point_id,
                   'fused_rank', points.source_key::bigint,
                   'fused_score', 1.0 / points.source_key::float8,
                   'contributions', pg_catalog.jsonb_build_array(
                       pg_catalog.jsonb_build_object(
                           'profile', 'real-model-smoke',
                           'rank', points.source_key::bigint,
                           'native_score', 1.0 / points.source_key::float8,
                           'weight', 1.0,
                           'contribution', 1.0 / points.source_key::float8
                       )
                   ),
                   'metadata', '[]'::jsonb
               )
               ORDER BY points.source_key::bigint
           ) AS value
      FROM pgcontext._visible_collection_points AS points
      JOIN pgcontext._visible_collections AS collections USING (collection_id)
     WHERE collections.collection_name = 'real_semantic_docs'
)
SELECT pgcontext.prepare_semantic_rerank(
           'real_semantic_docs',
           'body',
           'Which planet is known as the Red Planet?',
           candidates.value,
           'cross-encoder/ms-marco-MiniLM-L6-v2',
           1,
           60000,
           'require_reranker',
           NULL,
           false
       )::text
  FROM candidates;
SQL

"${python}" "${adapter}" --root "${runtime_root}" rerank \
  <"${envelope}" >"${response}"
response_json="$(tr -d '\n' <"${response}")"
rerank_result="$(${psql_command[@]} -qAt -F '|' -v response="${response_json}" <<'SQL'
WITH finalized AS (
    SELECT pgcontext.finalize_semantic_rerank(
               ((:'response')::jsonb->>'request_id')::bigint,
               (:'response')::jsonb,
               NULL
           ) AS value
)
SELECT value->>'status',
       value->'results'->0->>'occurrence_id',
       value->'results'->0 ? 'text'
  FROM finalized;
SQL
)"
[[ "${rerank_result}" == "reranked|2|f" ]] || {
  echo "real P12 PostgreSQL finalization returned an unexpected result: ${rerank_result}" >&2
  exit 1
}

cat >"${embedding_request}" <<'JSON'
{
  "chunks": [
    {
      "occurrence_id": 100,
      "citation": {"query": true},
      "text": "Which planet is known as the Red Planet?"
    },
    {
      "occurrence_id": 1,
      "citation": {"source_key": "banana", "start_byte": 0, "end_byte": 52},
      "text": "Bananas are yellow fruit grown in tropical climates."
    },
    {
      "occurrence_id": 2,
      "citation": {"source_key": "mars", "start_byte": 0, "end_byte": 78},
      "text": "Mars is known as the Red Planet because iron minerals oxidize on its surface."
    },
    {
      "occurrence_id": 3,
      "citation": {"source_key": "saturn", "start_byte": 0, "end_byte": 47},
      "text": "Saturn is a gas giant recognized by its rings."
    }
  ]
}
JSON
"${python}" "${adapter}" --root "${runtime_root}" embed \
  <"${embedding_request}" >"${embedding_response}"
embedding_json="$(tr -d '\n' <"${embedding_response}")"
embedding_result="$(${psql_command[@]} -qAt -F '|' -v payload="${embedding_json}" <<'SQL'
CREATE TABLE public.real_chunk_embeddings (
    occurrence_id bigint PRIMARY KEY,
    citation jsonb NOT NULL,
    embedding pgcontext.vector(384) NOT NULL
);
WITH chunks AS (
    SELECT value
      FROM pg_catalog.jsonb_array_elements((:'payload')::jsonb->'chunks') AS value
)
INSERT INTO public.real_chunk_embeddings (occurrence_id, citation, embedding)
SELECT (value->>'occurrence_id')::bigint,
       value->'citation',
       (value->'embedding')::text::pgcontext.vector
  FROM chunks
 WHERE (value->>'occurrence_id')::bigint <> 100;
CREATE INDEX real_chunk_embeddings_hnsw
    ON public.real_chunk_embeddings USING pgcontext_hnsw
    (embedding pgcontext.vector_hnsw_cosine_ops);
SET enable_seqscan = off;
WITH query_vector AS (
    SELECT (value->'embedding')::text::pgcontext.vector AS embedding
      FROM pg_catalog.jsonb_array_elements((:'payload')::jsonb->'chunks') AS value
     WHERE (value->>'occurrence_id')::bigint = 100
)
SELECT occurrence_id,
       citation->>'source_key',
       citation->>'start_byte',
       citation->>'end_byte'
  FROM public.real_chunk_embeddings
 ORDER BY embedding OPERATOR(pgcontext.<=>) (SELECT embedding FROM query_vector)
 LIMIT 1;
SQL
)"
[[ "${embedding_result}" == "2|mars|0|78" ]] || {
  echo "real P13 indexed citation retrieval returned an unexpected result: ${embedding_result}" >&2
  exit 1
}

embedding_plan="$(${psql_command[@]} -qAt -v payload="${embedding_json}" <<'SQL'
SET enable_seqscan = off;
EXPLAIN (COSTS OFF)
WITH query_vector AS (
    SELECT (value->'embedding')::text::pgcontext.vector AS embedding
      FROM pg_catalog.jsonb_array_elements((:'payload')::jsonb->'chunks') AS value
     WHERE (value->>'occurrence_id')::bigint = 100
)
SELECT occurrence_id
  FROM public.real_chunk_embeddings
 ORDER BY embedding OPERATOR(pgcontext.<=>) (SELECT embedding FROM query_vector)
 LIMIT 1;
SQL
)"
[[ "${embedding_plan}" == *"real_chunk_embeddings_hnsw"* ]] || {
  echo "real P13 retrieval did not use the HNSW index" >&2
  echo "${embedding_plan}" >&2
  exit 1
}

echo "real semantic PostgreSQL smoke passed (P12 finalized; P13 citation retrieved)"
