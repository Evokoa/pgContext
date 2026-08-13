enum JsonFrame {
    Array(Vec<Value>),
    Object {
        values: Map<String, Value>,
        pending_key: Option<String>,
    },
}

pub(crate) fn decode_bounded_jsonb(
    datum: pg_sys::Datum,
    is_null: bool,
    maximum_raw_bytes: usize,
    maximum_nodes: usize,
    maximum_depth: usize,
    message: &'static str,
) -> Option<JsonB> {
    if is_null {
        return None;
    }
    // SAFETY: PostgreSQL supplied a live JSONB datum; both operations inspect
    // or copy that datum in the current PostgreSQL memory context.
    let raw_bytes = unsafe { pg_sys::toast_raw_datum_size(datum) };
    if raw_bytes > maximum_raw_bytes {
        raise_sql_error(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, message);
    }
    // SAFETY: the datum is a non-null jsonb varlena by the generated SQL type.
    // The non-packed detoaster is required because the next step directly
    // dereferences the aligned four-byte `Jsonb` header and root container.
    let original = datum.cast_mut_ptr();
    let detoasted = unsafe { pg_sys::pg_detoast_datum(original) };
    let jsonb = detoasted.cast::<pg_sys::Jsonb>();
    // SAFETY: `jsonb` is a detoasted JSONB value with an initialized root.
    let mut iterator = unsafe { pg_sys::JsonbIteratorInit(&mut (*jsonb).root) };
    let mut frames = Vec::new();
    let mut root = None;
    let mut nodes = 0_usize;
    loop {
        let mut scalar = pg_sys::JsonbValue::default();
        // SAFETY: iterator and scalar remain live for this bounded traversal.
        let token = unsafe { pg_sys::JsonbIteratorNext(&mut iterator, &mut scalar, false) };
        if token == pg_sys::JsonbIteratorToken::WJB_DONE {
            break;
        }
        nodes = nodes.saturating_add(1);
        if nodes > maximum_nodes {
            free_detoasted_jsonb(original, detoasted);
            raise_sql_error(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, message);
        }
        match token {
            pg_sys::JsonbIteratorToken::WJB_BEGIN_ARRAY => {
                frames.push(JsonFrame::Array(Vec::new()));
            }
            pg_sys::JsonbIteratorToken::WJB_BEGIN_OBJECT => {
                frames.push(JsonFrame::Object {
                    values: Map::new(),
                    pending_key: None,
                });
            }
            pg_sys::JsonbIteratorToken::WJB_KEY => {
                let key = jsonb_string(&scalar, message);
                let Some(JsonFrame::Object { pending_key, .. }) = frames.last_mut() else {
                    free_detoasted_jsonb(original, detoasted);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
                };
                *pending_key = Some(key);
            }
            pg_sys::JsonbIteratorToken::WJB_VALUE | pg_sys::JsonbIteratorToken::WJB_ELEM => {
                let value = jsonb_scalar(&scalar, message);
                push_json_value(&mut frames, &mut root, value, message);
            }
            pg_sys::JsonbIteratorToken::WJB_END_ARRAY
            | pg_sys::JsonbIteratorToken::WJB_END_OBJECT => {
                let Some(frame) = frames.pop() else {
                    free_detoasted_jsonb(original, detoasted);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
                };
                let value = match frame {
                    JsonFrame::Array(values) => Value::Array(values),
                    JsonFrame::Object {
                        values,
                        pending_key: None,
                    } => Value::Object(values),
                    JsonFrame::Object { .. } => {
                        free_detoasted_jsonb(original, detoasted);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
                    }
                };
                push_json_value(&mut frames, &mut root, value, message);
            }
            _ => {
                free_detoasted_jsonb(original, detoasted);
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
            }
        }
        if frames.len() > maximum_depth {
            free_detoasted_jsonb(original, detoasted);
            raise_sql_error(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, message);
        }
    }
    free_detoasted_jsonb(original, detoasted);
    if !frames.is_empty() {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
    }
    root.map(JsonB).or_else(|| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
    })
}

fn jsonb_string(value: &pg_sys::JsonbValue, message: &'static str) -> String {
    if value.type_ != pg_sys::jbvType::jbvString {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
    }
    // SAFETY: the iterator marks this union arm as a live string.
    let string = unsafe { value.val.string };
    let length = usize::try_from(string.len).unwrap_or_else(|_| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
    });
    // SAFETY: JSONB string data is valid UTF-8 for exactly `length` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(string.val.cast::<u8>(), length) };
    std::str::from_utf8(bytes)
        .unwrap_or_else(|_| {
            raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
        })
        .to_owned()
}

fn jsonb_scalar(value: &pg_sys::JsonbValue, message: &'static str) -> Value {
    match value.type_ {
        pg_sys::jbvType::jbvNull => Value::Null,
        // SAFETY: the iterator marks this union arm as a live boolean.
        pg_sys::jbvType::jbvBool => Value::Bool(unsafe { value.val.boolean }),
        pg_sys::jbvType::jbvString => Value::String(jsonb_string(value, message)),
        pg_sys::jbvType::jbvNumeric => {
            // SAFETY: numeric_out accepts the live Numeric pointer carried by
            // the iterator value and returns a current-context C string.
            let output = unsafe {
                pgrx::direct_function_call::<&core::ffi::CStr>(
                    pg_sys::numeric_out,
                    &[Some(value.val.numeric.into())],
                )
            }
            .unwrap_or_else(|| {
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
            });
            let number = output
                .to_str()
                .ok()
                .and_then(|text| text.parse::<Number>().ok());
            // SAFETY: numeric_out allocated this string in the current context.
            unsafe { pg_sys::pfree(output.as_ptr().cast_mut().cast()) };
            Value::Number(number.unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "semantic rerank JSON numeric is outside the supported domain",
                );
            }))
        }
        _ => raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message),
    }
}

fn push_json_value(
    frames: &mut [JsonFrame],
    root: &mut Option<Value>,
    value: Value,
    message: &'static str,
) {
    match frames.last_mut() {
        Some(JsonFrame::Array(values)) => values.push(value),
        Some(JsonFrame::Object {
            values,
            pending_key,
        }) => {
            let Some(key) = pending_key.take() else {
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message);
            };
            values.insert(key, value);
        }
        None if root.is_none() => *root = Some(value),
        None => raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message),
    }
}

fn free_detoasted_jsonb(original: *mut pg_sys::varlena, detoasted: *mut pg_sys::varlena) {
    if detoasted != original {
        // SAFETY: PostgreSQL returned this copy from pg_detoast_datum_packed.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
}

macro_rules! bounded_json_argument {
    ($name:ident, $bytes:expr, $nodes:expr, $message:literal) => {
        #[derive(Debug)]
        pub struct $name(JsonB);

        impl FromDatum for $name {
            unsafe fn from_polymorphic_datum(
                datum: pg_sys::Datum,
                is_null: bool,
                _typoid: pg_sys::Oid,
            ) -> Option<Self> {
                decode_bounded_jsonb(
                    datum,
                    is_null,
                    $bytes,
                    $nodes,
                    MAX_RERANK_JSON_DEPTH,
                    $message,
                )
                .map(Self)
            }
        }

        impl IntoDatum for $name {
            fn into_datum(self) -> Option<pg_sys::Datum> {
                self.0.into_datum()
            }

            fn type_oid() -> pg_sys::Oid {
                JsonB::type_oid()
            }
        }

        // SAFETY: the generated wrapper declares jsonb and delegates decoding
        // to the bounded iterator above.
        unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for $name {
            unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
                let index = arg.index();
                // SAFETY: ArgAbi guarantees the declared jsonb SQL type.
                unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                        format!("semantic rerank JSON argument {index} is null"),
                    )
                })
            }
        }

        impl_sql_translatable!($name, "jsonb");
    };
}

bounded_json_argument!(
    BoundedCandidatesJson,
    MAX_RERANK_REQUEST_BYTES,
    MAX_RERANK_JSON_NODES,
    "semantic rerank candidates exceed the JSON allocation budget"
);
bounded_json_argument!(
    BoundedFilterJson,
    MAX_FILTER_JSON_RAW_BYTES,
    MAX_FILTER_JSON_NODES,
    "semantic rerank filter exceeds the JSON allocation budget"
);
bounded_json_argument!(
    BoundedResponseJson,
    MAX_RESPONSE_JSON_RAW_BYTES,
    MAX_RESPONSE_JSON_NODES,
    "semantic rerank response exceeds the JSON allocation budget"
);

fn bounded_text_from_datum(
    datum: pg_sys::Datum,
    is_null: bool,
    typoid: pg_sys::Oid,
    maximum: usize,
    message: &'static str,
) -> Option<String> {
    if is_null {
        return None;
    }
    // SAFETY: PostgreSQL supplied a live text datum. Raw TOAST size is read
    // before pgrx detoasts and allocates the Rust String.
    let raw_bytes = unsafe { pg_sys::toast_raw_datum_size(datum) };
    if raw_bytes > maximum.saturating_add(pg_sys::VARHDRSZ) {
        raise_sql_error(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, message);
    }
    // SAFETY: every caller maps its wrapper to SQL text.
    unsafe { String::from_polymorphic_datum(datum, false, typoid) }
}

macro_rules! bounded_text_argument {
    ($name:ident, $maximum:expr, $message:literal) => {
        #[derive(Debug)]
        pub struct $name(String);

        impl FromDatum for $name {
            unsafe fn from_polymorphic_datum(
                datum: pg_sys::Datum,
                is_null: bool,
                typoid: pg_sys::Oid,
            ) -> Option<Self> {
                bounded_text_from_datum(datum, is_null, typoid, $maximum, $message).map(Self)
            }
        }

        impl IntoDatum for $name {
            fn into_datum(self) -> Option<pg_sys::Datum> {
                self.0.into_datum()
            }

            fn type_oid() -> pg_sys::Oid {
                String::type_oid()
            }
        }

        // SAFETY: the generated wrapper declares text and delegates decoding to
        // the raw-size-checking `FromDatum` implementation above.
        unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for $name {
            unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
                let index = arg.index();
                // SAFETY: ArgAbi guarantees a datum of the declared text type.
                unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                        format!("semantic rerank text argument {index} is null"),
                    )
                })
            }
        }

        impl_sql_translatable!($name, "text");
    };
}

bounded_text_argument!(
    BoundedCollectionName,
    512,
    "collection name exceeds 512 bytes"
);
bounded_text_argument!(
    BoundedRerankSourceName,
    MAX_RERANK_SOURCE_NAME_BYTES,
    "semantic rerank source name exceeds 128 bytes"
);
bounded_text_argument!(
    BoundedRerankQuery,
    context_query::MAX_RERANK_QUERY_BYTES,
    "semantic rerank query exceeds 65536 bytes"
);
bounded_text_argument!(
    BoundedRerankModel,
    context_query::MAX_RERANK_MODEL_NAME_BYTES,
    "semantic rerank model exceeds 128 bytes"
);
bounded_text_argument!(
    BoundedFailurePolicy,
    64,
    "semantic rerank failure policy exceeds 64 bytes"
);
bounded_text_argument!(
    BoundedFailureReason,
    64,
    "semantic rerank failure reason exceeds 64 bytes"
);
bounded_text_argument!(
    BoundedColumnName,
    63,
    "semantic rerank column name exceeds 63 bytes"
);
