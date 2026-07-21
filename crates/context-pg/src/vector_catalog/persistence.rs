fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn vector_name_from_sql(vector_name: String) -> VectorName {
    match VectorName::new(vector_name) {
        Ok(vector_name) => vector_name,
        Err(error) => raise_core_error(error),
    }
}

fn sql_identifier_from_sql(identifier: String) -> SqlIdentifier {
    match SqlIdentifier::new(identifier) {
        Ok(identifier) => identifier,
        Err(error) => raise_core_error(error),
    }
}

fn dimensions_from_sql(dimensions: i32) -> VectorDimensions {
    let dimensions = match usize::try_from(dimensions) {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid sparse vector dimensions: {dimensions}"),
        ),
    };
    match VectorDimensions::new(dimensions) {
        Ok(dimensions) => dimensions,
        Err(error) => raise_core_error(error),
    }
}

fn json_object_from_sql(label: &str, value: JsonB) -> Value {
    let Value::Object(_) = value.0 else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} must be a JSON object"),
        );
    };
    value.0
}

fn require_collection(collection_name: &CollectionName) -> CollectionAcl {
    match find_collection(collection_name) {
        Some(collection) => collection,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("collection does not exist: {}", collection_name.as_str()),
        ),
    }
}

fn find_collection(collection_name: &CollectionName) -> Option<CollectionAcl> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT collection.collection_id,
                        collection.collection_name,
                        collection.owner_role,
                        role.rolname::text,
                        collection.source_table_oid,
                        collection.source_schema_name,
                        collection.source_table_name
                   FROM pgcontext._collections AS collection
                   JOIN pg_catalog.pg_roles AS role ON role.oid = collection.owner_role
                  WHERE collection.collection_name = $1",
                Some(1),
                &[collection_name.as_str().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to query collection catalog: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        let source_table_oid = optional_column::<pg_sys::Oid>(&row, 5, "source_table_oid");
        let source_schema_name = optional_column::<String>(&row, 6, "source_schema_name");
        let source_table_name = optional_column::<String>(&row, 7, "source_table_name");
        let source_table = match (source_table_oid, source_schema_name, source_table_name) {
            (Some(oid), Some(schema_name), Some(table_name)) => Some(TableResolution {
                oid,
                schema_name,
                table_name,
            }),
            (None, None, None) => None,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "collection catalog has partial source table metadata",
            ),
        };
        Some(CollectionAcl {
            id: required_column::<i64>(&row, 1, "collection_id"),
            name: required_column::<String>(&row, 2, "collection_name"),
            owner_role: required_column::<pg_sys::Oid>(&row, 3, "owner_role"),
            owner_name: required_column::<String>(&row, 4, "owner_name"),
            source_table,
        })
    })
}

fn require_collection_owner(collection: &CollectionAcl) {
    let session_user = session_user();
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')",
        &[session_user.as_str().into(), collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection owner: {error}"),
        )
    })
    .unwrap_or(false);

    if !is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for collection {}: owner is {}",
                collection.name, collection.owner_name
            ),
        );
    }
}

fn require_table_select_privilege(table: &TableResolution) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[session_user.as_str().into(), table.oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check source table privileges: {error}"),
        )
    })
    .unwrap_or(false);

    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for source table: {}.{}",
                table.schema_name, table.table_name
            ),
        );
    }
}

fn resolve_sparse_vector_column(
    table: &TableResolution,
    column: &SqlIdentifier,
) -> SparseVectorColumnResolution {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT attribute.attnum,
                    attribute.attname::text,
                    attribute.atttypid = 'public.sparsevec'::regtype AS is_sparsevec,
                    pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped",
            Some(1),
            &[table.oid.into(), column.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to inspect sparse vector column: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "sparse vector column does not exist on {}.{}: {}",
                    table.schema_name,
                    table.table_name,
                    column.as_str()
                ),
            );
        }

        let row = rows.first();
        let is_sparsevec = required_column::<bool>(&row, 3, "is_sparsevec");
        if !is_sparsevec {
            let data_type = required_column::<String>(&row, 4, "data_type");
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "sparse vector column must have type sparsevec: {}.{} is {data_type}",
                    table.table_name,
                    column.as_str()
                ),
            );
        }

        SparseVectorColumnResolution {
            attnum: required_column::<i16>(&row, 1, "vector_attnum"),
            column_name: required_column::<String>(&row, 2, "vector_column_name"),
        }
    })
}

fn select_vector_metadata(collection_id: i64, collection_name: &str) -> Vec<VectorMetadata> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT vector_name,
                        source_schema_name,
                        source_table_name,
                        vector_column_name,
                        dimensions,
                        metric,
                        hnsw_options,
                        quantization_options,
                        status
                   FROM pgcontext._collection_vectors
                  WHERE collection_id = $1
                  ORDER BY vector_name",
                None,
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to query vector metadata: {error}"),
                )
            });

        rows.into_iter()
            .map(|row| VectorMetadata {
                collection_name: collection_name.to_owned(),
                vector_name: required_heap_column::<String>(&row, 1, "vector_name"),
                table_schema: required_heap_column::<String>(&row, 2, "source_schema_name"),
                table_name: required_heap_column::<String>(&row, 3, "source_table_name"),
                vector_column: required_heap_column::<String>(&row, 4, "vector_column_name"),
                dimensions: required_heap_column::<i32>(&row, 5, "dimensions"),
                metric: distance_metric_from_catalog(
                    required_heap_column::<String>(&row, 6, "metric"),
                    "vector",
                ),
                hnsw_options: required_heap_column::<JsonB>(&row, 7, "hnsw_options").0,
                quantization_options: required_heap_column::<JsonB>(
                    &row,
                    8,
                    "quantization_options",
                )
                .0,
                status: vector_status_from_catalog(required_heap_column::<String>(
                    &row, 9, "status",
                )),
            })
            .collect()
    })
}

fn insert_sparse_vector_registration(
    collection: &CollectionAcl,
    vector_name: &VectorName,
    table: &TableResolution,
    column: &SparseVectorColumnResolution,
    dimensions: VectorDimensions,
    metric: DistanceMetric,
) -> Option<SparseVectorMetadata> {
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "WITH inserted AS (
                     INSERT INTO pgcontext._collection_sparse_vectors (
                         collection_id,
                         vector_name,
                         source_table_oid,
                         source_schema_name,
                         source_table_name,
                         vector_column_name,
                         vector_attnum,
                         dimensions,
                         metric
                     )
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                     ON CONFLICT DO NOTHING
                     RETURNING vector_name,
                               source_schema_name,
                               source_table_name,
                               vector_column_name,
                               dimensions,
                               metric,
                               storage_options,
                               index_options,
                               status
                 )
                 SELECT vector_name,
                        source_schema_name,
                        source_table_name,
                        vector_column_name,
                        dimensions,
                        metric,
                        storage_options,
                        index_options,
                        status
                   FROM inserted",
                Some(1),
                &[
                    collection.id.into(),
                    vector_name.as_str().into(),
                    table.oid.into(),
                    table.schema_name.as_str().into(),
                    table.table_name.as_str().into(),
                    column.column_name.as_str().into(),
                    column.attnum.into(),
                    i32::try_from(dimensions.get()).unwrap_or(i32::MAX).into(),
                    distance_metric_label(metric).into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to register sparse vector: {error}"),
                )
            });

        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(sparse_vector_metadata_from_tuple(
            &row,
            collection.name.clone(),
        ))
    })
}

fn select_sparse_vector_metadata(
    collection_id: i64,
    collection_name: &str,
) -> Vec<SparseVectorMetadata> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT vector_name,
                        source_schema_name,
                        source_table_name,
                        vector_column_name,
                        dimensions,
                        metric,
                        storage_options,
                        index_options,
                        status
                   FROM pgcontext._collection_sparse_vectors
                  WHERE collection_id = $1
                  ORDER BY vector_name",
                None,
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to query sparse vector metadata: {error}"),
                )
            });

        rows.into_iter()
            .map(|row| SparseVectorMetadata {
                collection_name: collection_name.to_owned(),
                vector_name: required_heap_column::<String>(&row, 1, "vector_name"),
                table_schema: required_heap_column::<String>(&row, 2, "source_schema_name"),
                table_name: required_heap_column::<String>(&row, 3, "source_table_name"),
                vector_column: required_heap_column::<String>(&row, 4, "vector_column_name"),
                dimensions: required_heap_column::<i32>(&row, 5, "dimensions"),
                metric: distance_metric_from_catalog(
                    required_heap_column::<String>(&row, 6, "metric"),
                    "sparse vector",
                ),
                storage_options: required_heap_column::<JsonB>(&row, 7, "storage_options").0,
                index_options: required_heap_column::<JsonB>(&row, 8, "index_options").0,
                status: vector_status_from_catalog(required_heap_column::<String>(
                    &row, 9, "status",
                )),
            })
            .collect()
    })
}

fn update_sparse_vector_metadata(
    collection_id: i64,
    collection_name: &str,
    vector_name: &VectorName,
    storage_options: &Value,
    index_options: &Value,
    status: VectorStatus,
) -> Option<SparseVectorMetadata> {
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "WITH updated AS (
                     UPDATE pgcontext._collection_sparse_vectors
                        SET storage_options = $3,
                            index_options = $4,
                            status = $5,
                            updated_at = pg_catalog.now()
                      WHERE collection_id = $1
                        AND vector_name = $2
                      RETURNING vector_name,
                                source_schema_name,
                                source_table_name,
                                vector_column_name,
                                dimensions,
                                metric,
                                storage_options,
                                index_options,
                                status
                 )
                 SELECT vector_name,
                        source_schema_name,
                        source_table_name,
                        vector_column_name,
                        dimensions,
                        metric,
                        storage_options,
                        index_options,
                        status
                   FROM updated",
                Some(1),
                &[
                    collection_id.into(),
                    vector_name.as_str().into(),
                    JsonB(storage_options.clone()).into(),
                    JsonB(index_options.clone()).into(),
                    status.as_sql().into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to update sparse vector metadata: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(sparse_vector_metadata_from_tuple(
            &row,
            collection_name.to_owned(),
        ))
    })
}

fn update_vector_metadata(
    collection_id: i64,
    collection_name: &str,
    vector_name: &VectorName,
    hnsw_options: &Value,
    quantization_options: &Value,
    status: VectorStatus,
) -> Option<VectorMetadata> {
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "WITH updated AS (
                     UPDATE pgcontext._collection_vectors
                        SET hnsw_options = $3,
                            quantization_options = $4,
                            status = $5,
                            updated_at = pg_catalog.now()
                      WHERE collection_id = $1
                        AND vector_name = $2
                      RETURNING vector_name,
                                source_schema_name,
                                source_table_name,
                                vector_column_name,
                                dimensions,
                                metric,
                                hnsw_options,
                                quantization_options,
                                status
                 ), collection_revision AS (
                     UPDATE pgcontext._collections
                       SET config_revision = config_revision + 1,
                            updated_at = pg_catalog.now()
                       FROM updated
                      WHERE collection_id = $1
                  RETURNING config_revision
                 ), stale_artifacts AS (
                     UPDATE pgcontext._artifact_segments AS artifacts
                        SET lifecycle_state = 'rebuild_required',
                            updated_at = pg_catalog.now()
                       FROM collection_revision
                      WHERE artifacts.collection_id = $1
                        AND artifacts.lifecycle_state = 'file_materialized'
                 )
                 SELECT vector_name,
                        source_schema_name,
                        source_table_name,
                        vector_column_name,
                        dimensions,
                        metric,
                        hnsw_options,
                        quantization_options,
                        status
                   FROM updated",
                Some(1),
                &[
                    collection_id.into(),
                    vector_name.as_str().into(),
                    JsonB(hnsw_options.clone()).into(),
                    JsonB(quantization_options.clone()).into(),
                    status.as_sql().into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to update vector metadata: {error}"),
                )
            });
        if rows.is_empty() {
            return None;
        }
        let row = rows.first();
        Some(VectorMetadata {
            collection_name: collection_name.to_owned(),
            vector_name: required_column::<String>(&row, 1, "vector_name"),
            table_schema: required_column::<String>(&row, 2, "source_schema_name"),
            table_name: required_column::<String>(&row, 3, "source_table_name"),
            vector_column: required_column::<String>(&row, 4, "vector_column_name"),
            dimensions: required_column::<i32>(&row, 5, "dimensions"),
            metric: distance_metric_from_catalog(
                required_column::<String>(&row, 6, "metric"),
                "vector",
            ),
            hnsw_options: required_column::<JsonB>(&row, 7, "hnsw_options").0,
            quantization_options: required_column::<JsonB>(&row, 8, "quantization_options").0,
            status: vector_status_from_catalog(required_column::<String>(&row, 9, "status")),
        })
    })
}

fn sparse_vector_metadata_from_tuple(
    row: &spi::SpiTupleTable<'_>,
    collection_name: String,
) -> SparseVectorMetadata {
    SparseVectorMetadata {
        collection_name,
        vector_name: required_column::<String>(row, 1, "vector_name"),
        table_schema: required_column::<String>(row, 2, "source_schema_name"),
        table_name: required_column::<String>(row, 3, "source_table_name"),
        vector_column: required_column::<String>(row, 4, "vector_column_name"),
        dimensions: required_column::<i32>(row, 5, "dimensions"),
        metric: distance_metric_from_catalog(
            required_column::<String>(row, 6, "metric"),
            "sparse vector",
        ),
        storage_options: required_column::<JsonB>(row, 7, "storage_options").0,
        index_options: required_column::<JsonB>(row, 8, "index_options").0,
        status: vector_status_from_catalog(required_column::<String>(row, 9, "status")),
    }
}

fn vector_metadata_row(
    row: VectorMetadata,
) -> (
    String,
    String,
    String,
    String,
    String,
    i32,
    String,
    JsonB,
    JsonB,
    String,
) {
    (
        row.collection_name,
        row.vector_name,
        row.table_schema,
        row.table_name,
        row.vector_column,
        row.dimensions,
        distance_metric_label(row.metric).to_owned(),
        JsonB(row.hnsw_options),
        JsonB(row.quantization_options),
        row.status.as_sql().to_owned(),
    )
}

fn sparse_vector_metadata_row(
    row: SparseVectorMetadata,
) -> (
    String,
    String,
    String,
    String,
    String,
    i32,
    String,
    JsonB,
    JsonB,
    String,
) {
    (
        row.collection_name,
        row.vector_name,
        row.table_schema,
        row.table_name,
        row.vector_column,
        row.dimensions,
        distance_metric_label(row.metric).to_owned(),
        JsonB(row.storage_options),
        JsonB(row.index_options),
        row.status.as_sql().to_owned(),
    )
}

fn session_user() -> String {
    match Spi::get_one::<String>("SELECT SESSION_USER::text") {
        Ok(Some(user)) => user,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "SESSION_USER returned null",
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read SESSION_USER: {error}"),
        ),
    }
}

fn optional_column<T>(
    row: &spi::SpiTupleTable<'_>,
    index: usize,
    column_name: &'static str,
) -> Option<T>
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(value) => value,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read vector metadata column {column_name}: {error}"),
        ),
    }
}

fn required_column<T>(row: &spi::SpiTupleTable<'_>, index: usize, column_name: &'static str) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("vector metadata column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read vector metadata column {column_name}: {error}"),
        ),
    }
}

fn required_heap_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
    index: usize,
    column_name: &'static str,
) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("vector metadata column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read vector metadata column {column_name}: {error}"),
        ),
    }
}
