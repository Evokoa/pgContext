use std::ffi::CStr;
use std::num::NonZeroUsize;

use pgrx::prelude::*;

use crate::error::raise_sql_error;

/// Copies one certified binary-compatible vector Datum into another column.
///
/// Trigger arguments are the 1-based source and target attribute numbers. The
/// trigger validates the exact pgvector/pgContext type pair and typmod on every
/// invocation before performing the raw Datum copy. It fires for every INSERT
/// or UPDATE so callers cannot corrupt a shadow column by assigning it directly.
#[pg_trigger]
fn _sync_pgvector_ownership_columns<'a>(
    trigger: &'a PgTrigger<'a>,
) -> Result<Option<PgHeapTuple<'a, AllocatedByRust>>, PgHeapTupleError> {
    let event = trigger.event();
    if !event.fired_before()
        || !event.fired_for_row()
        || !(event.fired_by_insert() || event.fired_by_update())
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_TRIGGERED_ACTION_EXCEPTION,
            "pgvector ownership synchronization must be a BEFORE ROW INSERT OR UPDATE trigger",
        );
    }
    let arguments = trigger.extra_args().unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid pgvector ownership trigger arguments: {error}"),
        )
    });
    if arguments.len() != 2 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "pgvector ownership synchronization requires source and target attribute numbers",
        );
    }
    let source_attnum = parse_attnum(&arguments[0], "source");
    let target_attnum = parse_attnum(&arguments[1], "target");
    if source_attnum == target_attnum {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "pgvector ownership synchronization source and target must differ",
        );
    }

    let Some(new_tuple) = trigger.new() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_TRIGGERED_ACTION_EXCEPTION,
            "pgvector ownership synchronization requires a NEW row",
        );
    };
    let source_attribute = new_tuple
        .get_attribute_by_index(source_attnum)
        .unwrap_or_else(|| invalid_attnum("source", source_attnum));
    let target_attribute = new_tuple
        .get_attribute_by_index(target_attnum)
        .unwrap_or_else(|| invalid_attnum("target", target_attnum));
    if source_attribute.attisdropped || target_attribute.attisdropped {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "pgvector ownership synchronization cannot use a dropped column",
        );
    }
    if source_attribute.atttypmod != target_attribute.atttypmod
        || !certified_type_pair(source_attribute.atttypid, target_attribute.atttypid)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            "pgvector ownership synchronization type or typmod binding is not certified",
        );
    }

    let trigger_data = trigger.trigger_data();
    let source_tuple = if event.fired_by_insert() {
        trigger_data.tg_trigtuple
    } else {
        trigger_data.tg_newtuple
    };
    if source_tuple.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_TRIGGERED_ACTION_EXCEPTION,
            "pgvector ownership synchronization received a null NEW tuple",
        );
    }
    let mut is_null = false;
    // SAFETY: PgTrigger validated TriggerData, NEW is present for this row
    // event, the attribute number was range-checked against its tuple
    // descriptor, and PostgreSQL owns the returned Datum until this call ends.
    let source_datum = unsafe {
        pg_sys::heap_getattr(
            source_tuple,
            i32::try_from(source_attnum.get()).unwrap_or(i32::MAX),
            (*trigger_data.tg_relation).rd_att,
            &mut is_null,
        )
    };
    let mut replacement = new_tuple.into_owned();
    // SAFETY: The exact source/target physical-layout pair and typmod were
    // certified above. heap_modify_tuple copies the by-reference varlena Datum
    // into the replacement tuple before the source tuple can be released.
    unsafe {
        replacement.set_by_index_unchecked(target_attnum, (!is_null).then_some(source_datum));
    }
    Ok(Some(replacement))
}

fn parse_attnum(value: &str, label: &str) -> NonZeroUsize {
    value
        .parse::<usize>()
        .ok()
        .and_then(NonZeroUsize::new)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("pgvector ownership trigger {label} attribute number is invalid"),
            )
        })
}

fn invalid_attnum(label: &str, value: NonZeroUsize) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
        format!(
            "pgvector ownership trigger {label} attribute {} does not exist",
            value.get()
        ),
    )
}

fn certified_type_pair(source: pg_sys::Oid, target: pg_sys::Oid) -> bool {
    // SAFETY: Static catalog identifiers are used only for syscache lookups.
    let canonical_vector = unsafe { named_type_oid(c"pgcontext", c"vector") };
    // SAFETY: Same static catalog lookup boundary.
    let canonical_halfvec = unsafe { named_type_oid(c"pgcontext", c"halfvec") };
    // SAFETY: pgvector is certified only when its extension-owned public types
    // resolve to these exact OIDs.
    let pgvector_vector = unsafe { certified_pgvector_type_oid(c"vector") };
    // SAFETY: Same certified pgvector lookup boundary.
    let pgvector_halfvec = unsafe { certified_pgvector_type_oid(c"halfvec") };

    matches!(
        (source, target),
        (source, target)
            if (source == pgvector_vector && target == canonical_vector)
                || (source == canonical_vector && target == pgvector_vector)
                || (source == pgvector_halfvec && target == canonical_halfvec)
                || (source == canonical_halfvec && target == pgvector_halfvec)
    )
}

unsafe fn certified_pgvector_type_oid(type_name: &'static CStr) -> pg_sys::Oid {
    // SAFETY: Static schema/type names are valid for the syscache lookup.
    let type_oid = unsafe { named_type_oid(c"public", type_name) };
    if type_oid != pg_sys::InvalidOid && crate::pgvector_compat::type_owned_by_pgvector(type_oid) {
        type_oid
    } else {
        pg_sys::InvalidOid
    }
}

unsafe fn named_type_oid(schema_name: &'static CStr, type_name: &'static CStr) -> pg_sys::Oid {
    // SAFETY: The names are static and PostgreSQL owns the namespace lookup.
    let namespace = unsafe { pg_sys::get_namespace_oid(schema_name.as_ptr(), true) };
    if namespace == pg_sys::InvalidOid {
        return pg_sys::InvalidOid;
    }
    let mut name = pg_sys::NameData::default();
    // SAFETY: Certified type names are shorter than NAMEDATALEN.
    unsafe { pg_sys::namestrcpy((&mut name) as pg_sys::Name, type_name.as_ptr()) };
    let oid_attribute =
        pg_sys::AttrNumber::try_from(pg_sys::Anum_pg_type_oid).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "pg_type OID attribute number is out of range",
            )
        });
    // SAFETY: TYPENAMENSP is keyed by initialized NameData and namespace OID.
    unsafe {
        pg_sys::GetSysCacheOid(
            pg_sys::SysCacheIdentifier::TYPENAMENSP.cast_signed(),
            oid_attribute,
            pg_sys::NameGetDatum(&name),
            pg_sys::ObjectIdGetDatum(namespace),
            pg_sys::Datum::from(0),
            pg_sys::Datum::from(0),
        )
    }
}
