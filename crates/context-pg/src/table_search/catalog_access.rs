pub(super) fn resolve_collection(collection_name: &CollectionName) -> SearchCollection {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id, owner_role, has_source_table
               FROM pgcontext._collection_acl
              WHERE collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection catalog: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }

        let row = rows.first();
        let has_source_table = spi_required_column::<bool>(&row, 3, "has_source_table");
        if !has_source_table {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        }

        SearchCollection {
            collection_id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
        }
    })
}

pub(crate) fn resolve_registered_vector(
    collection_name: &CollectionName,
    collection_id: i64,
) -> SearchVector {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_schema_name,
                    source_table_name,
                    source_table_oid,
                    vector_column_name,
                    vector_attnum,
                    hnsw_index_oid,
                    metric
               FROM pgcontext._visible_collection_vectors
              WHERE collection_id = $1
              ORDER BY vector_id",
            Some(2),
            &[collection_id.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query vector registration: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "collection has no registered vector: {}",
                    collection_name.as_str()
                ),
            );
        }
        if rows.len() > 1 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has multiple registered vectors; specify a vector name: {}",
                    collection_name.as_str()
                ),
            );
        }

        let row = rows.first();
        SearchVector {
            schema_name: spi_required_column::<String>(&row, 1, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 2, "source_table_name"),
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 3, "source_table_oid"),
            vector_column_name: spi_required_column::<String>(&row, 4, "vector_column_name"),
            vector_attnum: spi_required_column::<i16>(&row, 5, "vector_attnum"),
            hnsw_index_oid: spi_optional_column::<pg_sys::Oid>(&row, 6, "hnsw_index_oid"),
            metric: distance_metric_from_catalog(
                spi_required_column::<String>(&row, 7, "metric"),
                "vector",
            ),
        }
    })
}

pub(crate) fn validate_search_drift(collection_id: i64, registered_vector: &mut SearchVector) {
    if let Some(index_oid) = registered_vector.hnsw_index_oid {
        let valid = Spi::get_one_with_args::<bool>(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_class AS index_class
                 JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_class.relam
                 WHERE index_class.oid = $1 AND access_method.amname = 'pgcontext_hnsw'
             )",
            &[index_oid.into()],
        )
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate HNSW index binding: {error}"),
            )
        });
        if valid != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "registered HNSW index binding is unavailable",
            );
        }
    }
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    vector_attribute.attnum,
                    vector_attribute.attname::text,
                    vector_attribute.atttypid = 'public.vector'::regtype AS vector_is_valid,
                    id_attribute.attname IS NOT NULL AS id_exists
               FROM pg_catalog.pg_class AS class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
               LEFT JOIN pg_catalog.pg_attribute AS vector_attribute
                 ON vector_attribute.attrelid = class.oid
                AND vector_attribute.attname = $3
                AND vector_attribute.attnum > 0
                AND NOT vector_attribute.attisdropped
               LEFT JOIN pg_catalog.pg_attribute AS id_attribute
                 ON id_attribute.attrelid = class.oid
                AND id_attribute.attname = 'id'
                AND id_attribute.attnum > 0
                AND NOT id_attribute.attisdropped
              WHERE namespace.nspname = $1
                AND class.relname = $2
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[
                registered_vector.schema_name.as_str().into(),
                registered_vector.table_name.as_str().into(),
                registered_vector.vector_column_name.as_str().into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate search catalog drift: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!(
                    "registered source table drifted: {}.{}",
                    registered_vector.schema_name, registered_vector.table_name
                ),
            );
        }

        let row = rows.first();
        let current_table_oid = spi_required_column::<pg_sys::Oid>(&row, 1, "source_table_oid");
        let current_vector_attnum = spi_optional_column::<i16>(&row, 2, "vector_attnum");
        let vector_column = spi_optional_column::<String>(&row, 3, "vector_column_name");
        let vector_is_valid = spi_optional_column::<bool>(&row, 4, "vector_is_valid");
        if vector_column.is_none() || vector_is_valid != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "registered vector column drifted: {}.{}",
                    registered_vector.table_name, registered_vector.vector_column_name
                ),
            );
        }

        let Some(current_vector_attnum) = current_vector_attnum else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "validated vector column had no attnum",
            );
        };
        refresh_restored_search_metadata(
            collection_id,
            registered_vector,
            current_table_oid,
            current_vector_attnum,
        );

        let id_exists = spi_required_column::<bool>(&row, 5, "id_exists");
        if !id_exists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "source key column does not exist on {}.{}: id",
                    registered_vector.schema_name, registered_vector.table_name
                ),
            );
        }
    });
}

fn refresh_restored_search_metadata(
    collection_id: i64,
    registered_vector: &mut SearchVector,
    current_table_oid: pg_sys::Oid,
    current_vector_attnum: i16,
) {
    if registered_vector.table_oid == current_table_oid
        && registered_vector.vector_attnum == current_vector_attnum
    {
        return;
    }

    Spi::run_with_args(
        "UPDATE pgcontext._collections
            SET source_table_oid = $1,
                updated_at = pg_catalog.now()
          WHERE collection_id = $2",
        &[current_table_oid.into(), collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored collection metadata: {error}"),
        )
    });

    Spi::run_with_args(
        "UPDATE pgcontext._collection_vectors
            SET source_table_oid = $1,
                vector_attnum = $2,
                updated_at = pg_catalog.now()
          WHERE collection_id = $3
            AND vector_column_name = $4",
        &[
            current_table_oid.into(),
            current_vector_attnum.into(),
            collection_id.into(),
            registered_vector.vector_column_name.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored vector metadata: {error}"),
        )
    });

    Spi::run_with_args(
        "UPDATE pgcontext._collection_payload_columns AS payload
            SET source_table_oid = $1,
                column_attnum = attribute.attnum,
                updated_at = pg_catalog.now()
           FROM pg_catalog.pg_attribute AS attribute
          WHERE payload.collection_id = $2
            AND attribute.attrelid = $1
            AND attribute.attname = payload.column_name
            AND attribute.attnum > 0
            AND NOT attribute.attisdropped",
        &[current_table_oid.into(), collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored payload metadata: {error}"),
        )
    });

    registered_vector.table_oid = current_table_oid;
    registered_vector.vector_attnum = current_vector_attnum;
}
