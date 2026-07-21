#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_mmap_hnsw_restart}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

build_hnsw_payload_hex() {
    local point_rows

    point_rows="$(psql_db -At <<'SQL'
SELECT points.source_key || '|' || points.point_id
  FROM pgcontext._collection_points AS points
  JOIN pgcontext._collections AS collections USING (collection_id)
 WHERE collections.collection_name = 'mmap_restart_docs'
   AND points.source_key IN ('10', '20', '30')
 ORDER BY points.source_key::bigint
SQL
)"

    POINT_ROWS="${point_rows}" python3 <<'PY'
import os
import struct

point_ids = {}
for line in os.environ["POINT_ROWS"].splitlines():
    source_key, point_id = line.split("|", 1)
    point_ids[source_key] = int(point_id)

records = [
    ("10", (3.0, 0.0), (1,)),
    ("20", (1.0, 0.0), (0, 2)),
    ("30", (2.0, 0.0), (1,)),
]
if set(point_ids) != {source_key for source_key, _, _ in records}:
    raise SystemExit(f"missing point ids for restart fixture: {point_ids!r}")

payload = bytearray()
payload.extend(b"PGCTXHNS")
payload.extend(struct.pack("<IIII", 1, len(records), 2, 0))
for node_id, (source_key, vector, neighbors) in enumerate(records):
    payload.extend(struct.pack("<IIQ", node_id, len(neighbors), point_ids[source_key]))
    payload.extend(struct.pack("<ff", *vector))
    payload.extend(struct.pack("<" + "I" * len(neighbors), *neighbors))

print(payload.hex())
PY
}

validate_ready_artifact() {
    local phase="$1"
    local readiness

    readiness="$(psql_db -At <<'SQL' | tail -n 1
SELECT status || ':' || serving_ready::text
  FROM pgcontext.artifact_segment_serving_readiness('mmap_restart_docs', 4096)
 WHERE artifact_kind = 'mmap'
   AND artifact_name = 'view-a';
SQL
)"
    if [[ "${readiness}" != "ready:true" ]]; then
        echo "expected serving-ready mmap artifact after restart, got: ${readiness}" >&2
        exit 1
    fi
    printf 'mmap_artifact_serving_ready: %s\n' "${phase}"
}

validate_mmap_search() {
    local phase="$1"
    local ordered_keys

    ordered_keys="$(psql_db -At <<'SQL' | tail -n 1
SELECT string_agg(source_key, ',' ORDER BY score, point_id)
  FROM pgcontext.search_mmap_hnsw_artifact(
       'mmap_restart_docs',
       'view-a',
       '[0,0]'::vector,
       4096,
       3,
       2
  );
SQL
)"
    if [[ "${ordered_keys}" != "30,20" ]]; then
        echo "unexpected mmap artifact search order after source-table recheck: ${ordered_keys}" >&2
        exit 1
    fi
    printf 'mmap_artifact_source_recheck: %s\n' "${phase}"
}

validate_budget_failure() {
    local phase="$1"
    psql_db <<'SQL'
DO $$
DECLARE
    actual_sqlstate text;
    actual_message text;
BEGIN
    BEGIN
        PERFORM *
          FROM pgcontext.search_mmap_hnsw_artifact(
               'mmap_restart_docs',
               'view-a',
               '[0,0]'::vector,
               1,
               3,
               2
          );
        RAISE EXCEPTION 'expected mmap artifact search to reject tiny mapped-byte budget';
    EXCEPTION WHEN OTHERS THEN
        GET STACKED DIAGNOSTICS
            actual_sqlstate = RETURNED_SQLSTATE,
            actual_message = MESSAGE_TEXT;
        IF actual_sqlstate <> '55000' THEN
            RAISE EXCEPTION 'unexpected mmap budget SQLSTATE %, message %',
                actual_sqlstate,
                actual_message;
        END IF;
        IF actual_message NOT LIKE 'mmap artifact is not serving-ready: memory_budget_exceeded%' THEN
            RAISE EXCEPTION 'unexpected mmap budget message: %', actual_message;
        END IF;
    END;
END
$$;
SQL
    printf 'mmap_artifact_budget_rejected: %s\n' "${phase}"
}

validate_vacuum_recheck() {
    local phase="$1"
    local ordered_keys

    psql_db <<'SQL'
DELETE FROM public.mmap_restart_docs
 WHERE id = 20;
VACUUM (ANALYZE) public.mmap_restart_docs;
SQL

    ordered_keys="$(psql_db -At <<'SQL' | tail -n 1
SELECT string_agg(source_key, ',' ORDER BY score, point_id)
  FROM pgcontext.search_mmap_hnsw_artifact(
       'mmap_restart_docs',
       'view-a',
       '[0,0]'::vector,
       4096,
       3,
       3
  );
SQL
)"
    if [[ "${ordered_keys}" != "30,10" ]]; then
        echo "unexpected mmap artifact search order after vacuum recheck: ${ordered_keys}" >&2
        exit 1
    fi
    printf 'mmap_artifact_vacuum_recheck: %s\n' "${phase}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.mmap_restart_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.mmap_restart_docs (id, embedding, body)
VALUES
    (10, '[3,0]'::vector, 'artifact candidate ten'),
    (20, '[1,0]'::vector, 'artifact candidate twenty'),
    (30, '[2,0]'::vector, 'artifact candidate thirty');

SELECT pgcontext.create_collection('mmap_restart_docs', 'public.mmap_restart_docs');
SELECT pgcontext.register_vector(
    'mmap_restart_docs',
    'embedding',
    'embedding',
    2,
    'l2'
);
SELECT pgcontext.upsert_points('mmap_restart_docs', ARRAY['10', '20', '30']);
SQL

payload_hex="$(build_hnsw_payload_hex)"

psql_db <<SQL
SELECT build_job_id
  FROM pgcontext.start_build_job(
       'mmap_restart_docs',
       'mmap',
       'view-a',
       'public.mmap_restart_docs',
       3
  ) \gset
SELECT pgcontext.run_build_job(:build_job_id, 3);
SELECT artifact_id
  FROM pgcontext.publish_artifact_segment_file(
       :build_job_id,
       pgcontext.encode_artifact_segment('hnsw_graph', decode('${payload_hex}', 'hex'))
  );

UPDATE public.mmap_restart_docs
   SET embedding = '[0,0]'::vector
 WHERE id = 30;

CHECKPOINT;
SQL

validate_ready_artifact "before_restart"
validate_mmap_search "before_restart"
validate_budget_failure "before_restart"
cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"
validate_ready_artifact "after_restart"
validate_mmap_search "after_restart"
validate_budget_failure "after_restart"
validate_vacuum_recheck "after_restart"
