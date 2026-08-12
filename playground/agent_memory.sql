\set ON_ERROR_STOP on
\pset pager off

\echo ''
\echo '=== Agent memory playground ==='
\echo 'Question: What billing decisions did this user make previously?'
\echo ''

CREATE EXTENSION IF NOT EXISTS pgcontext;

SELECT pgcontext.drop_collection('agent_memory_decisions');
DROP TABLE IF EXISTS agent_decisions CASCADE;
DROP TABLE IF EXISTS agent_messages CASCADE;
DROP TABLE IF EXISTS agent_sessions CASCADE;
DROP TABLE IF EXISTS agent_users CASCADE;

CREATE TABLE agent_users (
    id text PRIMARY KEY,
    tenant_id text NOT NULL,
    display_name text NOT NULL
);

CREATE TABLE agent_sessions (
    id text PRIMARY KEY,
    user_id text NOT NULL REFERENCES agent_users(id),
    topic text NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE agent_messages (
    id text PRIMARY KEY,
    session_id text NOT NULL REFERENCES agent_sessions(id),
    user_id text NOT NULL REFERENCES agent_users(id),
    tenant_id text NOT NULL,
    role text NOT NULL,
    body text NOT NULL,
    embedding pgcontext.vector(4) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE agent_decisions (
    id text PRIMARY KEY,
    user_id text NOT NULL REFERENCES agent_users(id),
    session_id text REFERENCES agent_sessions(id),
    tenant_id text NOT NULL,
    category text NOT NULL,
    summary text NOT NULL,
    body text NOT NULL,
    decided_at timestamptz NOT NULL,
    embedding pgcontext.vector(4) NOT NULL
);

INSERT INTO agent_users (id, tenant_id, display_name) VALUES
    ('u-alice', 'acme', 'Alice Okello'),
    ('u-bob', 'other', 'Bob Mwangi');

INSERT INTO agent_sessions (id, user_id, topic, started_at) VALUES
    ('s-billing-1', 'u-alice', 'Enterprise billing review', '2026-07-01 09:00:00+00'),
    ('s-arch-1', 'u-alice', 'Retrieval architecture', '2026-07-08 14:00:00+00'),
    ('s-other-1', 'u-bob', 'Billing escalation', '2026-07-02 11:00:00+00');

INSERT INTO agent_messages (id, session_id, user_id, tenant_id, role, body, embedding, created_at) VALUES
    ('m-1', 's-billing-1', 'u-alice', 'acme', 'user',
     'Can we approve a partial refund for the billing dispute?',
     '[0.9,0.1,0,0]'::pgcontext.vector, '2026-07-01 09:05:00+00'),
    ('m-2', 's-billing-1', 'u-alice', 'acme', 'assistant',
     'Reviewing prior billing decisions and refund policy.',
     '[0.85,0.15,0,0]'::pgcontext.vector, '2026-07-01 09:06:00+00');

INSERT INTO agent_decisions (id, user_id, session_id, tenant_id, category, summary, body, decided_at, embedding) VALUES
    ('d-billing-refund', 'u-alice', 's-billing-1', 'acme', 'billing',
     'Approved partial enterprise refund',
     'Approved a partial refund for the enterprise billing dispute after policy review.',
     '2026-07-01 10:00:00+00', '[1,0,0,0]'::pgcontext.vector),
    ('d-arch-postgres', 'u-alice', 's-arch-1', 'acme', 'architecture',
     'Selected Postgres-native retrieval',
     'Selected Postgres-native hybrid retrieval instead of operating a separate vector database.',
     '2026-07-08 15:00:00+00', '[0,1,0,0]'::pgcontext.vector),
    ('d-onboarding-delay', 'u-alice', 's-arch-1', 'acme', 'onboarding',
     'Delayed multilingual rollout',
     'Delayed multilingual dataset onboarding to Q3 to finish annotation QA.',
     '2026-07-08 16:00:00+00', '[0,0,1,0]'::pgcontext.vector),
    ('d-other-billing', 'u-bob', 's-other-1', 'other', 'billing',
     'Denied refund for other tenant',
     'Denied refund request for a billing dispute in another tenant workspace.',
     '2026-07-02 12:00:00+00', '[1,0,0,0]'::pgcontext.vector);

SELECT * FROM pgcontext.create_collection(
    'agent_memory_decisions',
    'public.agent_decisions'
);
SELECT pgcontext.register_vector(
    'agent_memory_decisions', 'embedding', 'embedding', 4, 'cosine'
);
SELECT pgcontext.register_filter_column(
    'agent_memory_decisions', 'tenant_id', 'tenant_id'
);
SELECT pgcontext.register_filter_column(
    'agent_memory_decisions', 'user_id', 'user_id'
);
SELECT pgcontext.register_filter_column(
    'agent_memory_decisions', 'category', 'category'
);
SELECT pgcontext.upsert_points(
    'agent_memory_decisions',
    ARRAY['d-billing-refund', 'd-arch-postgres', 'd-onboarding-delay', 'd-other-billing']
);

\echo ''
\echo 'Filtered dense memory search (tenant + user + category)'
SELECT source_key, score
FROM pgcontext.search(
    'agent_memory_decisions',
    '[0.95,0.05,0,0]'::pgcontext.vector,
    '{
       "must": [
         {"key": "tenant_id", "match": "acme"},
         {"key": "user_id", "match": "u-alice"},
         {"key": "category", "match": "billing"}
       ]
     }',
    5
);

\echo ''
\echo 'Hybrid memory search (dense + full-text RRF)'
SELECT source_key, score
FROM pgcontext.query(
    'agent_memory_decisions',
    '[0.95,0.05,0,0]'::pgcontext.vector,
    'billing refund',
    'body',
    5
);

\echo ''
\echo 'Context pack for downstream LLM prompt assembly'
SELECT
    hits.rank,
    hits.score,
    d.summary,
    d.body,
    d.decided_at,
    s.topic AS session_topic
FROM (
    SELECT
        ROW_NUMBER() OVER (ORDER BY score DESC, source_key) AS rank,
        source_key,
        score
    FROM pgcontext.query(
        'agent_memory_decisions',
        '[0.95,0.05,0,0]'::pgcontext.vector,
        'billing refund',
        'body',
        5
    )
    WHERE source_key IN (
        SELECT id
        FROM agent_decisions
        WHERE tenant_id = 'acme'
          AND user_id = 'u-alice'
    )
) hits
JOIN agent_decisions d ON d.id = hits.source_key
LEFT JOIN agent_sessions s ON s.id = d.session_id
ORDER BY hits.rank;
