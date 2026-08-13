use context_core::{ExactFirstFailure, ExactFirstReadiness};

use super::introspection::SourceKeyBinding;
use super::*;

#[derive(Clone, Debug)]
pub(super) struct CollectionRegistration {
    pub(super) collection_id: i64,
    pub(super) owner_role: pg_sys::Oid,
    pub(super) source_table_oid: pg_sys::Oid,
    pub(super) source_schema_name: String,
    pub(super) source_table_name: String,
}

#[derive(Clone, Debug)]
pub(super) struct ExistingRegistration {
    pub(super) registration_revision: i64,
    pub(super) source_table_oid: pg_sys::Oid,
    pub(super) source_schema_name: String,
    pub(super) source_table_name: String,
    pub(super) source_key_column_name: String,
    pub(super) source_key_attnum: i16,
    pub(super) source_key_type_oid: pg_sys::Oid,
    pub(super) source_key_typmod: i32,
    pub(super) source_key_collation_oid: i64,
    pub(super) source_key_index_oid: pg_sys::Oid,
    pub(super) specification_sha256: Vec<u8>,
    pub(super) readiness_state: String,
    pub(super) readiness_reason: String,
    pub(super) current_plan_revision: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StoredRegistration {
    pub(super) registration_revision: i64,
    pub(super) readiness_state: ExactFirstState,
    pub(super) readiness_reason: ExactFirstReason,
    pub(super) current_plan_revision: Option<i64>,
}

#[derive(Clone, Debug)]
pub(super) struct RegistrationReadiness {
    pub(super) registration_revision: i64,
    pub(super) readiness: ExactFirstReadiness,
    pub(super) plan_revision: Option<i64>,
    pub(super) build_job_id: Option<i64>,
}

pub(super) fn ensure_collection(
    collection_name: &CollectionName,
    source: &ResolvedSource,
) -> CollectionRegistration {
    if let Some(collection) = load_collection_optional(collection_name) {
        if collection.source_table_oid != source.oid
            || collection.source_schema_name != source.schema_name
            || collection.source_table_name != source.table_name
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "exact-first collection source table does not match the requested relation",
            );
        }
        return collection;
    }
    let qualified = format!("{}.{}", source.schema_name, source.table_name);
    Spi::connect(|client| {
        client
            .select(
                "SELECT collection_id
                   FROM pgcontext.create_collection($1, $2)",
                Some(1),
                &[collection_name.as_str().into(), qualified.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to create exact-first collection: {error}"),
                )
            });
    });
    load_collection(collection_name)
}

pub(super) fn load_collection(collection_name: &CollectionName) -> CollectionRegistration {
    load_collection_optional(collection_name).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "exact-first collection does not exist",
        )
    })
}

fn load_collection_optional(collection_name: &CollectionName) -> Option<CollectionRegistration> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT collection_id, owner_role, source_table_oid,
                        source_schema_name, source_table_name
                   FROM pgcontext._collections
                  WHERE collection_name = $1",
                Some(1),
                &[collection_name.as_str().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first collection: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(CollectionRegistration {
            collection_id: required(row.get::<i64>(1).unwrap_or(None), "collection_id"),
            owner_role: required(row.get::<pg_sys::Oid>(2).unwrap_or(None), "owner_role"),
            source_table_oid: required(
                row.get::<pg_sys::Oid>(3).unwrap_or(None),
                "collection_source_table_oid",
            ),
            source_schema_name: required(
                row.get::<String>(4).unwrap_or(None),
                "collection_source_schema_name",
            ),
            source_table_name: required(
                row.get::<String>(5).unwrap_or(None),
                "collection_source_table_name",
            ),
        })
    })
}

pub(super) fn load_existing_registration(collection_id: i64) -> Option<ExistingRegistration> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registration_revision, source_table_oid,
                        source_schema_name, source_table_name,
                        source_key_column_name, source_key_attnum,
                        source_key_type_oid, source_key_typmod,
                        source_key_collation_oid, source_key_index_oid,
                        specification_sha256, readiness_state, readiness_reason,
                        current_plan_revision
                   FROM pgcontext._exact_first_registrations
                  WHERE collection_id = $1",
                Some(1),
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first registration: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(ExistingRegistration {
            registration_revision: required(
                row.get::<i64>(1).unwrap_or(None),
                "registration_revision",
            ),
            source_table_oid: required(
                row.get::<pg_sys::Oid>(2).unwrap_or(None),
                "source_table_oid",
            ),
            source_schema_name: required(
                row.get::<String>(3).unwrap_or(None),
                "source_schema_name",
            ),
            source_table_name: required(row.get::<String>(4).unwrap_or(None), "source_table_name"),
            source_key_column_name: required(
                row.get::<String>(5).unwrap_or(None),
                "source_key_column_name",
            ),
            source_key_attnum: required(row.get::<i16>(6).unwrap_or(None), "source_key_attnum"),
            source_key_type_oid: required(
                row.get::<pg_sys::Oid>(7).unwrap_or(None),
                "source_key_type_oid",
            ),
            source_key_typmod: required(row.get::<i32>(8).unwrap_or(None), "source_key_typmod"),
            source_key_collation_oid: required(
                row.get::<i64>(9).unwrap_or(None),
                "source_key_collation_oid",
            ),
            source_key_index_oid: required(
                row.get::<pg_sys::Oid>(10).unwrap_or(None),
                "source_key_index_oid",
            ),
            specification_sha256: required(
                row.get::<Vec<u8>>(11).unwrap_or(None),
                "specification_sha256",
            ),
            readiness_state: required(row.get::<String>(12).unwrap_or(None), "readiness_state"),
            readiness_reason: required(row.get::<String>(13).unwrap_or(None), "readiness_reason"),
            current_plan_revision: row.get::<i64>(14).unwrap_or(None),
        })
    })
}

pub(super) fn insert_registration(
    collection: &CollectionRegistration,
    source: &ResolvedSource,
    source_key: &SourceKeyBinding,
    bindings: &[ColumnBinding],
    digest: &[u8],
) -> StoredRegistration {
    let (system_identifier, database_oid) = origin_identity();
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "INSERT INTO pgcontext._exact_first_registrations (
                     collection_id, registration_system_identifier,
                     registration_database_oid, source_table_oid,
                     source_schema_name, source_table_name,
                     source_key_column_name, source_key_attnum,
                     source_key_type_oid, source_key_typmod,
                     source_key_collation_oid, source_key_index_oid,
                     specification_version, specification_sha256
                 ) VALUES (
                     $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14
                 )
                 ON CONFLICT (collection_id) DO NOTHING
                 RETURNING exact_first_registration_id, registration_revision",
                Some(1),
                &[
                    collection.collection_id.into(),
                    system_identifier.into(),
                    database_oid.into(),
                    source.oid.into(),
                    source.schema_name.as_str().into(),
                    source.table_name.as_str().into(),
                    source_key.column.name.as_str().into(),
                    source_key.column.attnum.into(),
                    source_key.column.type_oid.into(),
                    source_key.column.typmod.into(),
                    i64::from(source_key.column.collation_oid.to_u32()).into(),
                    source_key.unique_index_oid.into(),
                    REGISTRATION_VERSION.into(),
                    digest.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to insert exact-first registration: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                "exact-first registration changed concurrently",
            );
        }
        let row = rows.first();
        let registration_id = required(
            row.get::<i64>(1).unwrap_or(None),
            "exact_first_registration_id",
        );
        let revision = required(row.get::<i64>(2).unwrap_or(None), "registration_revision");
        for binding in bindings {
            client
                .update(
                    "INSERT INTO pgcontext._exact_first_columns (
                         exact_first_registration_id, binding_ordinal,
                         binding_name, binding_kind, column_name, column_attnum,
                         column_type_oid, column_typmod, column_collation_oid,
                         dimensions, metric, text_configuration_oid, normalization
                     ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                    None,
                    &[
                        registration_id.into(),
                        binding.ordinal.into(),
                        binding.name.as_str().into(),
                        binding.kind.as_str().into(),
                        binding.column.name.as_str().into(),
                        binding.column.attnum.into(),
                        binding.column.type_oid.into(),
                        binding.column.typmod.into(),
                        i64::from(binding.column.collation_oid.to_u32()).into(),
                        nullable(binding.dimensions),
                        nullable_text(binding.metric.as_deref()),
                        nullable_oid(binding.text_configuration_oid),
                        nullable_text(binding.normalization.as_deref()),
                    ],
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to insert exact-first column binding: {error}"),
                    )
                });
        }
        StoredRegistration {
            registration_revision: revision,
            readiness_state: ExactFirstState::ExactOnly,
            readiness_reason: ExactFirstReason::CurrentExactPath,
            current_plan_revision: None,
        }
    })
}

pub(super) fn load_registration_readiness(
    collection: &CollectionRegistration,
) -> RegistrationReadiness {
    refresh_registration_after_logical_restore(collection.collection_id);
    Spi::connect_mut(|client| {
        let rows = client
            .select(
                "SELECT registrations.exact_first_registration_id,
                        registrations.registration_revision,
                        registrations.source_table_oid,
                        registrations.source_schema_name,
                        registrations.source_table_name,
                        registrations.source_key_attnum,
                        registrations.source_key_type_oid,
                        registrations.source_key_typmod,
                        registrations.source_key_collation_oid,
                        registrations.source_key_index_oid,
                        registrations.current_plan_revision,
                        registrations.readiness_state,
                        registrations.readiness_reason,
                        class.oid IS NOT NULL
                            AND namespace.nspname = registrations.source_schema_name
                            AND class.relname = registrations.source_table_name
                            AS relation_current,
                        key_attribute.attnum IS NOT NULL
                            AND key_attribute.attname::text =
                                registrations.source_key_column_name
                            AND key_attribute.atttypid = registrations.source_key_type_oid
                            AND key_attribute.atttypmod = registrations.source_key_typmod
                            AND key_attribute.attcollation::bigint = registrations.source_key_collation_oid
                            AND key_attribute.attnotnull
                            AS source_key_current,
                        COALESCE(key_index.indisunique AND key_index.indisvalid
                            AND key_index.indisready AND key_index.indislive
                            AND key_index.indimmediate AND key_index.indpred IS NULL
                            AND key_index.indexprs IS NULL AND key_index.indnkeyatts = 1, false)
                            AS source_key_index_current,
                        NOT EXISTS (
                            SELECT 1
                              FROM pgcontext._exact_first_columns AS columns
                              LEFT JOIN pg_catalog.pg_attribute AS attribute
                                ON attribute.attrelid = registrations.source_table_oid
                               AND attribute.attnum = columns.column_attnum
                               AND NOT attribute.attisdropped
                             WHERE columns.exact_first_registration_id =
                                   registrations.exact_first_registration_id
                               AND (
                                   attribute.attnum IS NULL
                                   OR attribute.attname::text <> columns.column_name
                                   OR attribute.atttypid <> columns.column_type_oid
                                   OR attribute.atttypmod <> columns.column_typmod
                                   OR attribute.attcollation::bigint <> columns.column_collation_oid
                               )
                        ) AS source_columns_current,
                        plan_jobs.status AS plan_status,
                        plan_jobs.build_job_id,
                        EXISTS (
                            SELECT 1
                              FROM pgcontext._exact_first_targets AS targets
                              JOIN pg_catalog.pg_index AS target_index
                                ON target_index.indexrelid = targets.index_oid
                             WHERE targets.exact_first_registration_id =
                                   registrations.exact_first_registration_id
                               AND targets.lifecycle_state = 'current'
                               AND targets.structurally_validated
                               AND targets.exact_first_plan_id = plans.exact_first_plan_id
                               AND target_index.indisvalid AND target_index.indisready
                               AND target_index.indislive
                        ) AS optimization_ready
                   FROM pgcontext._exact_first_registrations AS registrations
                   LEFT JOIN pg_catalog.pg_class AS class
                     ON class.oid = registrations.source_table_oid
                   LEFT JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = class.relnamespace
                   LEFT JOIN pg_catalog.pg_attribute AS key_attribute
                     ON key_attribute.attrelid = registrations.source_table_oid
                    AND key_attribute.attnum = registrations.source_key_attnum
                    AND NOT key_attribute.attisdropped
                   LEFT JOIN pg_catalog.pg_index AS key_index
                     ON key_index.indexrelid = registrations.source_key_index_oid
                    AND key_index.indrelid = registrations.source_table_oid
                   LEFT JOIN pgcontext._exact_first_plans AS plans
                     ON plans.exact_first_registration_id = registrations.exact_first_registration_id
                    AND plans.plan_revision = registrations.current_plan_revision
                   LEFT JOIN pgcontext._exact_first_plan_jobs AS plan_jobs
                     ON plan_jobs.exact_first_plan_id = plans.exact_first_plan_id
                  WHERE registrations.collection_id = $1",
                Some(1),
                &[collection.collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to derive exact-first readiness: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "exact-first registration does not exist",
            );
        }
        let row = rows.first();
        let registration_id = required(row.get::<i64>(1).unwrap_or(None), "registration_id");
        let registration_revision =
            required(row.get::<i64>(2).unwrap_or(None), "registration_revision");
        let relation_current = required(row.get::<bool>(14).unwrap_or(None), "relation_current");
        let key_current = required(row.get::<bool>(15).unwrap_or(None), "source_key_current")
            && required(
                row.get::<bool>(16).unwrap_or(None),
                "source_key_index_current",
            );
        let columns_current = required(
            row.get::<bool>(17).unwrap_or(None),
            "source_columns_current",
        );
        let plan_status = row.get::<String>(18).unwrap_or(None);
        let build_job_id = row.get::<i64>(19).unwrap_or(None);
        let optimization_ready =
            required(row.get::<bool>(20).unwrap_or(None), "optimization_ready");
        let failure = match plan_status.as_deref() {
            Some("failed") => Some(ExactFirstFailure::BuildFailed),
            Some("cancelled") => Some(ExactFirstFailure::BuildCancelled),
            _ => None,
        };
        let build_active = matches!(
            plan_status.as_deref(),
            Some("queued" | "building" | "validating")
        );
        let readiness = derive_exact_first_readiness(ExactFirstReadinessFacts {
            relation_current,
            source_key_current: key_current,
            source_columns_current: columns_current,
            configuration_current: true,
            exact_path_available: relation_current && key_current && columns_current,
            build_active,
            optimization_ready,
            failure,
        });
        client
            .update(
                "UPDATE pgcontext._exact_first_registrations
                    SET readiness_state = $1, readiness_reason = $2,
                        updated_at = pg_catalog.now()
                  WHERE exact_first_registration_id = $3",
                None,
                &[
                    readiness.state.as_catalog().into(),
                    readiness.reason.as_catalog().into(),
                    registration_id.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to persist exact-first readiness: {error}"),
                )
            });
        RegistrationReadiness {
            registration_revision,
            readiness,
            plan_revision: row.get::<i64>(11).unwrap_or(None),
            build_job_id,
        }
    })
}

fn refresh_registration_after_logical_restore(collection_id: i64) {
    #[derive(Debug)]
    struct RestoreIdentity {
        registration_id: i64,
        system_identifier: i64,
        database_oid: pg_sys::Oid,
        schema_name: String,
        table_name: String,
        key_column_name: String,
    }

    let identity = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT exact_first_registration_id,
                        registration_system_identifier,
                        registration_database_oid,
                        source_schema_name, source_table_name,
                        source_key_column_name
                   FROM pgcontext._exact_first_registrations
                  WHERE collection_id = $1",
                Some(1),
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first restore identity: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(RestoreIdentity {
            registration_id: required(row.get::<i64>(1).unwrap_or(None), "registration_id"),
            system_identifier: required(
                row.get::<i64>(2).unwrap_or(None),
                "registration_system_identifier",
            ),
            database_oid: required(
                row.get::<pg_sys::Oid>(3).unwrap_or(None),
                "registration_database_oid",
            ),
            schema_name: required(row.get::<String>(4).unwrap_or(None), "source_schema_name"),
            table_name: required(row.get::<String>(5).unwrap_or(None), "source_table_name"),
            key_column_name: required(
                row.get::<String>(6).unwrap_or(None),
                "source_key_column_name",
            ),
        })
    });
    let Some(identity) = identity else {
        return;
    };
    let (system_identifier, database_oid) = origin_identity();
    if identity.system_identifier == system_identifier && identity.database_oid == database_oid {
        return;
    }

    let qualified_source = format!("{}.{}", identity.schema_name, identity.table_name);
    let source = resolve_source(&qualified_source);
    require_source_select(&source);
    let columns = inspect_columns(source.oid);
    let source_key = resolve_source_key(&source, &columns, &identity.key_column_name);
    let specifications = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT binding_name, column_name, binding_kind,
                        dimensions, metric, normalization,
                        text_configuration_oid
                   FROM pgcontext._exact_first_columns
                  WHERE exact_first_registration_id = $1
                  ORDER BY binding_ordinal",
                Some(i64::try_from(MAX_COLUMNS).unwrap_or(i64::MAX)),
                &[identity.registration_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first restore bindings: {error}"),
                )
            });
        rows.into_iter()
            .map(|row| {
                if row.get::<pg_sys::Oid>(7).unwrap_or(None).is_some() {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                        "logical restore of uncertified exact-first text bindings is unsupported",
                    );
                }
                ExactFirstBindingSpecification {
                    name: required(row.get::<String>(1).unwrap_or(None), "binding_name"),
                    column: required(row.get::<String>(2).unwrap_or(None), "column_name"),
                    kind: required(row.get::<String>(3).unwrap_or(None), "binding_kind"),
                    dimensions: row.get::<i32>(4).unwrap_or(None),
                    metric: row.get::<String>(5).unwrap_or(None),
                    text_configuration: None,
                    normalization: row.get::<String>(6).unwrap_or(None),
                }
            })
            .collect::<Vec<_>>()
    });
    if specifications.is_empty() || specifications.len() > MAX_COLUMNS {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first restored binding count is invalid",
        );
    }
    let bindings = resolve_bindings(&columns, &specifications);

    Spi::connect_mut(|client| {
        client
            .select(
                "SELECT pgcontext._refresh_collection_source_table($1)",
                Some(1),
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to refresh restored collection source: {error}"),
                )
            });
        client
            .update(
                "UPDATE pgcontext._exact_first_registrations
                    SET registration_system_identifier = $1,
                        registration_database_oid = $2,
                        source_table_oid = $3,
                        source_key_attnum = $4,
                        source_key_type_oid = $5,
                        source_key_typmod = $6,
                        source_key_collation_oid = $7,
                        source_key_index_oid = $8,
                        readiness_state = 'exact_only',
                        readiness_reason = 'current_exact_path',
                        updated_at = pg_catalog.now()
                  WHERE exact_first_registration_id = $9
                    AND registration_system_identifier = $10
                    AND registration_database_oid::bigint = $11",
                None,
                &[
                    system_identifier.into(),
                    database_oid.into(),
                    source.oid.into(),
                    source_key.column.attnum.into(),
                    source_key.column.type_oid.into(),
                    source_key.column.typmod.into(),
                    i64::from(source_key.column.collation_oid.to_u32()).into(),
                    source_key.unique_index_oid.into(),
                    identity.registration_id.into(),
                    identity.system_identifier.into(),
                    i64::from(identity.database_oid.to_u32()).into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to refresh restored exact-first registration: {error}"),
                )
            });
        for binding in &bindings {
            client
                .update(
                    "UPDATE pgcontext._exact_first_columns
                        SET column_attnum = $1, column_type_oid = $2,
                            column_typmod = $3, column_collation_oid = $4
                      WHERE exact_first_registration_id = $5
                        AND binding_ordinal = $6",
                    None,
                    &[
                        binding.column.attnum.into(),
                        binding.column.type_oid.into(),
                        binding.column.typmod.into(),
                        i64::from(binding.column.collation_oid.to_u32()).into(),
                        identity.registration_id.into(),
                        binding.ordinal.into(),
                    ],
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to refresh restored exact-first binding: {error}"),
                    )
                });
        }
    });
}

fn origin_identity() -> (i64, pg_sys::Oid) {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT control.system_identifier::bigint, database.oid
                   FROM pg_catalog.pg_control_system() AS control
                   JOIN pg_catalog.pg_database AS database
                     ON database.datname = pg_catalog.current_database()",
                Some(1),
                &[],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first origin identity: {error}"),
                )
            });
        let row = rows.first();
        (
            required(row.get::<i64>(1).unwrap_or(None), "system_identifier"),
            required(row.get::<pg_sys::Oid>(2).unwrap_or(None), "database_oid"),
        )
    })
}

fn nullable(value: Option<i32>) -> DatumWithOid<'static> {
    value.map_or_else(|| None::<i32>.into(), Into::into)
}

fn nullable_oid(value: Option<pg_sys::Oid>) -> DatumWithOid<'static> {
    value.map_or_else(|| None::<pg_sys::Oid>.into(), Into::into)
}

fn nullable_text(value: Option<&str>) -> DatumWithOid<'_> {
    value.map_or_else(|| None::<String>.into(), Into::into)
}
