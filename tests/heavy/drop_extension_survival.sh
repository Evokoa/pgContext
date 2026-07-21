#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_drop_extension_survival}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.source_without_extension_types (
    id bigint PRIMARY KEY,
    body text NOT NULL
);

INSERT INTO public.source_without_extension_types (id, body)
VALUES (1, 'survives extension drop');

SELECT * FROM pgcontext.create_collection(
    'drop_survival_docs',
    'public.source_without_extension_types'
);

DROP EXTENSION pgcontext;

DO $$
DECLARE
    source_rows bigint;
BEGIN
    SELECT count(*) INTO source_rows FROM public.source_without_extension_types;
    IF source_rows <> 1 THEN
        RAISE EXCEPTION 'user-owned source table did not survive extension drop';
    END IF;

    IF pg_catalog.to_regnamespace('pgcontext') IS NOT NULL THEN
        RAISE EXCEPTION 'pgcontext schema still exists after extension drop';
    END IF;
END
$$;
SQL
