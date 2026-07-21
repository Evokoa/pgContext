// Catalog value decoding, checked conversions, and recall validation helpers.

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index status catalog column was unexpectedly null: {column_name}"),
        ),
    }
}

fn i64_to_usize(value: i64, column_name: &'static str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("index memory {column_name} exceeds supported range: {value}"),
        )
    })
}

fn i32_to_usize(value: i32, column_name: &'static str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("index memory {column_name} must be non-negative: {value}"),
        )
    })
}

fn checked_i64_product(values: &[usize], column_name: &'static str) -> i64 {
    let product = values
        .iter()
        .try_fold(1usize, |product, value| product.checked_mul(*value));
    let Some(product) = product else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("index memory {column_name} exceeds supported range"),
        );
    };
    usize_to_i64(product, column_name)
}

fn validate_min_recall(min_recall: f64) {
    if !min_recall.is_finite() || !(0.0..=1.0).contains(&min_recall) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("min_recall must be between 0 and 1 inclusive: {min_recall}"),
        );
    }
}

fn point_id_set(point_ids: Vec<i64>, argument_name: &'static str) -> BTreeSet<i64> {
    let mut ids = BTreeSet::new();
    for point_id in point_ids {
        if point_id < 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("{argument_name} must not contain negative point IDs: {point_id}"),
            );
        }
        ids.insert(point_id);
    }
    ids
}

fn usize_to_i64(value: usize, column_name: &'static str) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("recall check {column_name} exceeds bigint range: {value}"),
        )
    })
}

fn recall_ratio(intersection_count: usize, exact_count: usize) -> f64 {
    let intersection_count = u32::try_from(intersection_count).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "recall check intersection_count exceeds supported range: {intersection_count}"
            ),
        )
    });
    let exact_count = u32::try_from(exact_count).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("recall check exact_count exceeds supported range: {exact_count}"),
        )
    });

    f64::from(intersection_count) / f64::from(exact_count)
}
