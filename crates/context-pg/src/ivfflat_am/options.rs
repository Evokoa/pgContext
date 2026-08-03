//! Reloptions for the `pgcontext_ivfflat` access method.

use pgrx::prelude::*;
use std::ffi::CStr;
use std::mem::{offset_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use context_codec::CodecSpec;

use crate::error::raise_sql_error;
use crate::hnsw_am::ffi_boundary::PgCallbackScope;

const DEFAULT_LISTS: i32 = 100;
const MAX_LISTS: i32 = 32_768;
const QUANTIZATION_NONE: i32 = 0;
const QUANTIZATION_SQ8: i32 = 1;
const QUANTIZATION_PQ: i32 = 2;
const DEFAULT_PQ_SUBVECTOR_DIMENSIONS: i32 = 8;
const IVFFLAT_RELOPT_LOCKMODE: pg_sys::LOCKMODE = pg_sys::AccessExclusiveLock.cast_signed();
static IVFFLAT_RELOPT_KIND: AtomicU32 = AtomicU32::new(0);

static mut QUANTIZATION_MEMBERS: [pg_sys::relopt_enum_elt_def; 4] = [
    pg_sys::relopt_enum_elt_def {
        string_val: c"none".as_ptr(),
        symbol_val: QUANTIZATION_NONE,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: c"sq8".as_ptr(),
        symbol_val: QUANTIZATION_SQ8,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: c"pq".as_ptr(),
        symbol_val: QUANTIZATION_PQ,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: ptr::null(),
        symbol_val: 0,
    },
];

#[repr(C)]
struct IvfflatRelOptions {
    vl_len_: i32,
    lists: i32,
    quantization: i32,
    pq_subvector_dimensions: i32,
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies the reloptions datum and owns the returned bytea
// in the active memory context. Static option metadata and checked offsets
// describe exactly `IvfflatRelOptions`.
pub(super) unsafe extern "C-unwind" fn pgcontext_ivfflat_options(
    reloptions: pg_sys::Datum,
    validate: bool,
) -> *mut pg_sys::bytea {
    // SAFETY: PostgreSQL entered this guarded reloptions callback.
    let _scope = unsafe { PgCallbackScope::new() };
    // SAFETY: registration is backend-local and uses only static C strings.
    let kind = unsafe { ivfflat_reloption_kind() };
    let elements = [
        parse_element(
            c"lists",
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            offset_of!(IvfflatRelOptions, lists),
        ),
        parse_element(
            c"quantization",
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            offset_of!(IvfflatRelOptions, quantization),
        ),
        parse_element(
            c"pq_subvector_dimensions",
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            offset_of!(IvfflatRelOptions, pq_subvector_dimensions),
        ),
    ];
    // SAFETY: kind, struct size, and field offset were registered together.
    unsafe {
        pg_sys::build_reloptions(
            reloptions,
            validate,
            kind,
            size_of::<IvfflatRelOptions>(),
            elements.as_ptr(),
            i32::try_from(elements.len()).unwrap_or(i32::MAX),
        )
        .cast::<pg_sys::bytea>()
    }
}

fn parse_element(
    name: &'static CStr,
    option_type: pg_sys::relopt_type::Type,
    offset: usize,
) -> pg_sys::relopt_parse_elt {
    pg_sys::relopt_parse_elt {
        optname: name.as_ptr(),
        opttype: option_type,
        offset: i32::try_from(offset).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat reloption offset exceeds PostgreSQL integer range",
            )
        }),
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    }
}

/// Returns the validated list count stored on an index relation.
///
/// # Safety
///
/// `index_relation` must be a live relation owned by this access method.
pub(super) unsafe fn list_count(index_relation: pg_sys::Relation) -> usize {
    if index_relation.is_null() {
        return DEFAULT_LISTS as usize;
    }
    // SAFETY: this AM's options callback owns `rd_options` layout.
    let options = unsafe { (*index_relation).rd_options };
    if options.is_null() {
        return DEFAULT_LISTS as usize;
    }
    // SAFETY: non-null `rd_options` was returned for `IvfflatRelOptions`.
    let lists = unsafe { (*options.cast::<IvfflatRelOptions>()).lists };
    usize::try_from(lists).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "stored IVFFlat list count is negative",
        )
    })
}

/// Returns the normalized codec configuration stored on an index relation.
///
/// # Safety
///
/// `index_relation` must be a live relation owned by this access method.
pub(super) unsafe fn codec_spec(index_relation: pg_sys::Relation) -> CodecSpec {
    if index_relation.is_null() {
        return CodecSpec::plain();
    }
    // SAFETY: this AM's options callback owns `rd_options` layout.
    let options = unsafe { (*index_relation).rd_options };
    if options.is_null() {
        return CodecSpec::plain();
    }
    // SAFETY: non-null `rd_options` was returned for `IvfflatRelOptions`.
    let options = unsafe { &*options.cast::<IvfflatRelOptions>() };
    let spec = match options.quantization {
        QUANTIZATION_NONE => Ok(CodecSpec::plain()),
        QUANTIZATION_SQ8 => CodecSpec::scalar(256, None),
        QUANTIZATION_PQ => CodecSpec::product(
            usize::try_from(options.pq_subvector_dimensions).unwrap_or(0),
            256,
            16,
        ),
        _ => unreachable!("PostgreSQL reloptions reject unknown IVFFlat codec values"),
    };
    spec.unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid IVFFlat codec configuration: {error}"),
        )
    })
}

unsafe fn ivfflat_reloption_kind() -> pg_sys::relopt_kind::Type {
    let existing = IVFFLAT_RELOPT_KIND.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }
    // SAFETY: PostgreSQL registers static backend-lifetime metadata.
    let kind = unsafe { pg_sys::add_reloption_kind() };
    // SAFETY: all pointers refer to static C strings and the bounds are valid.
    unsafe {
        pg_sys::add_int_reloption(
            kind,
            c"lists".as_ptr(),
            c"Number of IVFFlat centroid lists.".as_ptr(),
            DEFAULT_LISTS,
            1,
            MAX_LISTS,
            IVFFLAT_RELOPT_LOCKMODE,
        );
        pg_sys::add_enum_reloption(
            kind,
            c"quantization".as_ptr(),
            c"Quantized candidate encoding for pgcontext_ivfflat.".as_ptr(),
            ptr::addr_of_mut!(QUANTIZATION_MEMBERS).cast::<pg_sys::relopt_enum_elt_def>(),
            QUANTIZATION_NONE,
            c"Valid values are none, sq8, and pq.".as_ptr(),
            IVFFLAT_RELOPT_LOCKMODE,
        );
        pg_sys::add_int_reloption(
            kind,
            c"pq_subvector_dimensions".as_ptr(),
            c"Product-quantization subvector width for pgcontext_ivfflat.".as_ptr(),
            DEFAULT_PQ_SUBVECTOR_DIMENSIONS,
            1,
            i32::MAX,
            IVFFLAT_RELOPT_LOCKMODE,
        );
    }
    IVFFLAT_RELOPT_KIND.store(kind, Ordering::Release);
    kind
}
