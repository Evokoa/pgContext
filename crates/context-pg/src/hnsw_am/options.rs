//! Reloptions for the `pgcontext_hnsw` access method.

use pgrx::prelude::*;
use serde_json::Value as JsonValue;
use std::ffi::CStr;
use std::mem::{offset_of, size_of};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::error::raise_sql_error;

const HNSW_QUANTIZATION_NONE: i32 = 0;
const HNSW_QUANTIZATION_SCALAR: i32 = 1;
const HNSW_QUANTIZATION_SQ8: i32 = 2;
const HNSW_QUANTIZATION_PQ: i32 = 3;
const HNSW_SCALAR_DEFAULT_MIN: f64 = -1.0;
const HNSW_SCALAR_DEFAULT_MAX: f64 = 1.0;
const HNSW_SCALAR_DEFAULT_LEVELS: i32 = 256;
const HNSW_SCALAR_MIN_LEVELS: i32 = 2;
const HNSW_SCALAR_MAX_LEVELS: i32 = 256;
const HNSW_PQ_DEFAULT_SUBVECTOR_DIMENSIONS: i32 = 0;
const HNSW_NO_STRING_OFFSET: i32 = 0;
const HNSW_RELOPT_LOCKMODE: pg_sys::LOCKMODE = pg_sys::AccessExclusiveLock.cast_signed();
const HNSW_QUANTIZATION_NONE_U16: u16 = 0;
const HNSW_QUANTIZATION_SCALAR_U16: u16 = 1;
const HNSW_QUANTIZATION_SQ8_U16: u16 = 2;
const HNSW_QUANTIZATION_PQ_U16: u16 = 3;
pub(super) const HNSW_QUANTIZATION_METADATA_VERSION: u16 = 1;
static HNSW_RELOPT_KIND: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswQuantizationMetadata {
    pub mode: u16,
    pub version: u16,
    pub scalar_min_bits: u64,
    pub scalar_max_bits: u64,
    pub scalar_levels: u32,
    pub pq_subvector_dimensions: u32,
    pub pq_codebooks_hash: u64,
}

impl HnswQuantizationMetadata {
    pub(super) const fn none() -> Self {
        Self {
            mode: HNSW_QUANTIZATION_NONE_U16,
            version: HNSW_QUANTIZATION_METADATA_VERSION,
            scalar_min_bits: 0,
            scalar_max_bits: 0,
            scalar_levels: 0,
            pq_subvector_dimensions: 0,
            pq_codebooks_hash: 0,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct HnswRelOptions {
    vl_len_: i32,
    quantization: i32,
    scalar_min: f64,
    scalar_max: f64,
    scalar_levels: i32,
    pq_subvector_dimensions: i32,
    pq_codebooks_offset: i32,
}

static mut HNSW_QUANTIZATION_MEMBERS: [pg_sys::relopt_enum_elt_def; 5] = [
    pg_sys::relopt_enum_elt_def {
        string_val: c"none".as_ptr(),
        symbol_val: HNSW_QUANTIZATION_NONE,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: c"scalar".as_ptr(),
        symbol_val: HNSW_QUANTIZATION_SCALAR,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: c"sq8".as_ptr(),
        symbol_val: HNSW_QUANTIZATION_SQ8,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: c"pq".as_ptr(),
        symbol_val: HNSW_QUANTIZATION_PQ,
    },
    pg_sys::relopt_enum_elt_def {
        string_val: ptr::null(),
        symbol_val: 0,
    },
];

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL calls this access-method callback with a reloptions Datum
// and expects a bytea allocated in the active memory context. All option names,
// descriptions, and enum members are static C strings, and parsed validation
// reads only the struct returned by PostgreSQL's reloptions parser.
pub(super) unsafe extern "C-unwind" fn pgcontext_hnsw_options(
    reloptions: pg_sys::Datum,
    validate: bool,
) -> *mut pg_sys::bytea {
    self::hnsw_options_safe(reloptions, validate)
}

pub(super) fn hnsw_options_safe(reloptions: pg_sys::Datum, validate: bool) -> *mut pg_sys::bytea {
    // SAFETY: Reloption kind registration uses static names/descriptions and is
    // cached per backend before parsing any index options.
    let kind = unsafe { hnsw_reloption_kind() };
    let parse_elements = [
        hnsw_relopt_parse_elt(
            c"quantization",
            pg_sys::relopt_type::RELOPT_TYPE_ENUM,
            offset_of!(HnswRelOptions, quantization),
        ),
        hnsw_relopt_parse_elt(
            c"scalar_min",
            pg_sys::relopt_type::RELOPT_TYPE_REAL,
            offset_of!(HnswRelOptions, scalar_min),
        ),
        hnsw_relopt_parse_elt(
            c"scalar_max",
            pg_sys::relopt_type::RELOPT_TYPE_REAL,
            offset_of!(HnswRelOptions, scalar_max),
        ),
        hnsw_relopt_parse_elt(
            c"scalar_levels",
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            offset_of!(HnswRelOptions, scalar_levels),
        ),
        hnsw_relopt_parse_elt(
            c"pq_subvector_dimensions",
            pg_sys::relopt_type::RELOPT_TYPE_INT,
            offset_of!(HnswRelOptions, pq_subvector_dimensions),
        ),
        hnsw_relopt_parse_elt(
            c"pq_codebooks",
            pg_sys::relopt_type::RELOPT_TYPE_STRING,
            offset_of!(HnswRelOptions, pq_codebooks_offset),
        ),
    ];

    // SAFETY: `kind` was registered for this access method, the parse element
    // offsets target fields within `HnswRelOptions`, and PostgreSQL allocates
    // the returned varlena in the current memory context.
    let parsed = unsafe {
        pg_sys::build_reloptions(
            reloptions,
            validate,
            kind,
            size_of::<HnswRelOptions>(),
            parse_elements.as_ptr(),
            usize_to_pg_i32(parse_elements.len(), "HNSW reloption parse element count"),
        )
    };

    if validate && !parsed.is_null() {
        // SAFETY: `build_reloptions` returned a populated `HnswRelOptions`
        // layout because we passed that exact struct size and field offsets.
        unsafe { validate_hnsw_reloptions(parsed.cast::<HnswRelOptions>()) };
    }

    parsed.cast::<pg_sys::bytea>()
}

pub(super) unsafe fn hnsw_quantization_metadata(
    index_relation: pg_sys::Relation,
) -> HnswQuantizationMetadata {
    if index_relation.is_null() {
        return HnswQuantizationMetadata::none();
    }

    // SAFETY: PostgreSQL owns the relation pointer for the duration of the AM
    // callback, and `rd_options` points to the parsed bytea returned by
    // `pgcontext_hnsw_options` when reloptions are present.
    let reloptions = unsafe { (*index_relation).rd_options };
    if reloptions.is_null() {
        return HnswQuantizationMetadata::none();
    }

    // SAFETY: `rd_options` uses the `HnswRelOptions` layout because this access
    // method registered `pgcontext_hnsw_options` as its options callback.
    let options = unsafe { &*reloptions.cast::<HnswRelOptions>() };
    // SAFETY: The reloptions value has the validated HNSW layout established
    // above, including any quantization-specific trailing string storage.
    unsafe { metadata_from_reloptions(options) }
}

fn hnsw_relopt_parse_elt(
    optname: &'static CStr,
    opttype: pg_sys::relopt_type::Type,
    offset: usize,
) -> pg_sys::relopt_parse_elt {
    pg_sys::relopt_parse_elt {
        optname: optname.as_ptr(),
        opttype,
        offset: usize_to_pg_i32(offset, "HNSW reloption struct offset"),
        // PostgreSQL 18 added `isset_offset` to `relopt_parse_elt`: an optional
        // offset to a bool in the parse target that PostgreSQL sets when the
        // option is supplied explicitly rather than defaulted. PostgreSQL only
        // honours it when the value is greater than zero, so 0 disables that
        // write. pgContext does not track "was this explicitly set" (it
        // validates HNSW reloptions itself), so 0 is both correct and inert.
        // The field does not exist before PG18, hence the cfg gate.
        #[cfg(feature = "pg18")]
        isset_offset: 0,
    }
}

unsafe fn metadata_from_reloptions(options: &HnswRelOptions) -> HnswQuantizationMetadata {
    match options.quantization {
        HNSW_QUANTIZATION_SCALAR | HNSW_QUANTIZATION_SQ8 => HnswQuantizationMetadata {
            mode: quantization_mode_to_u16(options.quantization),
            version: HNSW_QUANTIZATION_METADATA_VERSION,
            scalar_min_bits: options.scalar_min.to_bits(),
            scalar_max_bits: options.scalar_max.to_bits(),
            scalar_levels: positive_i32_to_u32(options.scalar_levels, "scalar_levels"),
            pq_subvector_dimensions: 0,
            pq_codebooks_hash: 0,
        },
        HNSW_QUANTIZATION_PQ => {
            // SAFETY: PQ reloptions were validated before PostgreSQL made the
            // relation available to build callbacks.
            let codebooks = unsafe { hnsw_reloption_string(options, options.pq_codebooks_offset) }
                .unwrap_or_default();
            HnswQuantizationMetadata {
                mode: HNSW_QUANTIZATION_PQ_U16,
                version: HNSW_QUANTIZATION_METADATA_VERSION,
                scalar_min_bits: 0,
                scalar_max_bits: 0,
                scalar_levels: 0,
                pq_subvector_dimensions: positive_i32_to_u32(
                    options.pq_subvector_dimensions,
                    "pq_subvector_dimensions",
                ),
                pq_codebooks_hash: fnv1a64(codebooks.as_bytes()),
            }
        }
        _ => HnswQuantizationMetadata::none(),
    }
}

unsafe fn hnsw_reloption_kind() -> pg_sys::relopt_kind::Type {
    let existing = HNSW_RELOPT_KIND.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }

    // SAFETY: PostgreSQL reloptions registration runs inside a backend process.
    // The strings and enum member table have static lifetime, and the returned
    // kind is cached for later callbacks in this backend.
    let kind = unsafe { pg_sys::add_reloption_kind() };
    // SAFETY: These registrations use static names, descriptions, and enum
    // members; every offset targets the corresponding HnswRelOptions field.
    unsafe {
        pg_sys::add_enum_reloption(
            kind,
            c"quantization".as_ptr(),
            c"Quantized candidate encoding for pgcontext_hnsw.".as_ptr(),
            ptr::addr_of_mut!(HNSW_QUANTIZATION_MEMBERS).cast::<pg_sys::relopt_enum_elt_def>(),
            HNSW_QUANTIZATION_NONE,
            c"Valid values are none, scalar, sq8, and pq.".as_ptr(),
            HNSW_RELOPT_LOCKMODE,
        );
        pg_sys::add_real_reloption(
            kind,
            c"scalar_min".as_ptr(),
            c"Minimum value for scalar or SQ8 HNSW quantization.".as_ptr(),
            HNSW_SCALAR_DEFAULT_MIN,
            f64::MIN,
            f64::MAX,
            HNSW_RELOPT_LOCKMODE,
        );
        pg_sys::add_real_reloption(
            kind,
            c"scalar_max".as_ptr(),
            c"Maximum value for scalar or SQ8 HNSW quantization.".as_ptr(),
            HNSW_SCALAR_DEFAULT_MAX,
            f64::MIN,
            f64::MAX,
            HNSW_RELOPT_LOCKMODE,
        );
        pg_sys::add_int_reloption(
            kind,
            c"scalar_levels".as_ptr(),
            c"Number of scalar or SQ8 reconstruction levels.".as_ptr(),
            HNSW_SCALAR_DEFAULT_LEVELS,
            HNSW_SCALAR_MIN_LEVELS,
            HNSW_SCALAR_MAX_LEVELS,
            HNSW_RELOPT_LOCKMODE,
        );
        pg_sys::add_int_reloption(
            kind,
            c"pq_subvector_dimensions".as_ptr(),
            c"Product-quantization subvector width for pgcontext_hnsw.".as_ptr(),
            HNSW_PQ_DEFAULT_SUBVECTOR_DIMENSIONS,
            0,
            i32::MAX,
            HNSW_RELOPT_LOCKMODE,
        );
        pg_sys::add_string_reloption(
            kind,
            c"pq_codebooks".as_ptr(),
            c"JSON product-quantization centroid codebooks for pgcontext_hnsw.".as_ptr(),
            ptr::null(),
            None,
            HNSW_RELOPT_LOCKMODE,
        );
    }

    HNSW_RELOPT_KIND.store(kind, Ordering::Release);
    kind
}

unsafe fn validate_hnsw_reloptions(options: *const HnswRelOptions) {
    if options.is_null() {
        return;
    }

    // SAFETY: The caller passes the parsed reloptions pointer returned by
    // PostgreSQL for the `HnswRelOptions` layout.
    let options = unsafe { &*options };
    match options.quantization {
        HNSW_QUANTIZATION_NONE => {}
        HNSW_QUANTIZATION_SCALAR | HNSW_QUANTIZATION_SQ8 => {
            validate_scalar_hnsw_reloptions(options);
        }
        HNSW_QUANTIZATION_PQ => {
            // SAFETY: `options` is the parsed reloptions allocation and string
            // offsets are validated by `validate_pq_hnsw_reloptions`.
            unsafe { validate_pq_hnsw_reloptions(options) };
        }
        _ => raise_invalid_hnsw_reloption("unsupported pgcontext_hnsw quantization mode"),
    }
}

fn validate_scalar_hnsw_reloptions(options: &HnswRelOptions) {
    if !options.scalar_min.is_finite() || !options.scalar_max.is_finite() {
        raise_invalid_hnsw_reloption("scalar_min and scalar_max must be finite");
    }
    if options.scalar_min >= options.scalar_max {
        raise_invalid_hnsw_reloption("scalar_min must be less than scalar_max");
    }
    if !(HNSW_SCALAR_MIN_LEVELS..=HNSW_SCALAR_MAX_LEVELS).contains(&options.scalar_levels) {
        raise_invalid_hnsw_reloption("scalar_levels must be between 2 and 256");
    }
}

unsafe fn validate_pq_hnsw_reloptions(options: &HnswRelOptions) {
    if options.pq_subvector_dimensions <= 0 {
        raise_invalid_hnsw_reloption(
            "pq_subvector_dimensions must be positive when quantization is pq",
        );
    }

    // SAFETY: String offsets in PostgreSQL reloptions are byte offsets inside
    // the returned varlena struct when the option was supplied.
    let Some(codebooks) = (unsafe { hnsw_reloption_string(options, options.pq_codebooks_offset) })
    else {
        raise_invalid_hnsw_reloption("pq_codebooks is required when quantization is pq");
    };

    let Ok(JsonValue::Array(codebooks_json)) = serde_json::from_str::<JsonValue>(codebooks) else {
        raise_invalid_hnsw_reloption("pq_codebooks must be a JSON array");
    };
    if codebooks_json.is_empty() {
        raise_invalid_hnsw_reloption("pq_codebooks must contain at least one codebook");
    }
}

unsafe fn hnsw_reloption_string(options: &HnswRelOptions, offset: i32) -> Option<&str> {
    if offset <= HNSW_NO_STRING_OFFSET {
        return None;
    }

    let base = ptr::from_ref(options).cast::<u8>();
    let offset = usize::try_from(offset).unwrap_or_else(|_| {
        raise_invalid_hnsw_reloption("string reloption offset exceeds platform pointer range")
    });
    // SAFETY: PostgreSQL stores string reloptions as NUL-terminated strings at
    // the parsed offset inside the same varlena allocation.
    let value = unsafe { CStr::from_ptr(base.add(offset).cast()) };
    value.to_str().ok()
}

fn raise_invalid_hnsw_reloption(message: &'static str) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message)
}

fn usize_to_pg_i32(value: usize, context: &'static str) -> i32 {
    i32::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{context} exceeds PostgreSQL integer range: {value}"),
        )
    })
}

fn quantization_mode_to_u16(mode: i32) -> u16 {
    match mode {
        HNSW_QUANTIZATION_NONE => HNSW_QUANTIZATION_NONE_U16,
        HNSW_QUANTIZATION_SCALAR => HNSW_QUANTIZATION_SCALAR_U16,
        HNSW_QUANTIZATION_SQ8 => HNSW_QUANTIZATION_SQ8_U16,
        HNSW_QUANTIZATION_PQ => HNSW_QUANTIZATION_PQ_U16,
        _ => raise_invalid_hnsw_reloption("unsupported pgcontext_hnsw quantization mode"),
    }
}

fn positive_i32_to_u32(value: i32, option_name: &'static str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| {
        raise_invalid_hnsw_reloption(match option_name {
            "scalar_levels" => "scalar_levels must be positive",
            "pq_subvector_dimensions" => "pq_subvector_dimensions must be positive",
            _ => "integer reloption must be positive",
        })
    })
}

const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
}
