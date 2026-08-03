//! Validation for SQL-visible vector metadata JSON.

use serde_json::Value;

use context_codec::{CodecKind, CodecSpec, ScalarBounds};

const CURRENT_QUANTIZATION_METADATA_VERSION: u64 = 1;
const MIN_SCALAR_LEVELS: u64 = 2;
const MAX_SCALAR_LEVELS: u64 = 256;

const DEFAULT_PRODUCT_CENTROIDS: usize = 16;
const DEFAULT_PRODUCT_ITERATIONS: usize = 8;

pub(crate) fn validate_quantization_options(value: &Value) -> Result<(), String> {
    parse_quantization_options(value).map(|_| ())
}

pub(crate) fn parse_quantization_options(value: &Value) -> Result<CodecSpec, String> {
    let Some(options) = value.as_object() else {
        return Err("quantization_options must be a JSON object".to_owned());
    };

    if let Some(version) = options
        .get("metadata_version")
        .or_else(|| options.get("version"))
    {
        let version = version.as_u64().ok_or_else(|| {
            "quantization_options metadata_version must be a positive integer".to_owned()
        })?;
        if version == 0 || version > CURRENT_QUANTIZATION_METADATA_VERSION {
            return Err(format!(
                "unsupported quantization_options metadata_version: {version}"
            ));
        }
    }

    let mode = options.get("mode").map_or(Ok("none"), |mode| {
        mode.as_str()
            .ok_or_else(|| "quantization_options mode must be a string".to_owned())
    })?;
    match mode {
        "none" => Ok(CodecSpec::plain()),
        "binary" => Ok(CodecSpec::binary()),
        "scalar" | "sq8" => parse_scalar_quantization_options(options),
        "pq" => parse_product_quantization_options(options),
        _ => Err(format!("unsupported quantization_options mode: {mode}")),
    }
}

pub(crate) fn quantization_codec_kind(value: &Value) -> Result<CodecKind, String> {
    parse_quantization_options(value).map(CodecSpec::kind)
}

fn parse_scalar_quantization_options(
    options: &serde_json::Map<String, Value>,
) -> Result<CodecSpec, String> {
    let levels = if let Some(levels) = options.get("levels") {
        let levels = levels
            .as_u64()
            .ok_or_else(|| "quantization_options levels must be a positive integer".to_owned())?;
        if !(MIN_SCALAR_LEVELS..=MAX_SCALAR_LEVELS).contains(&levels) {
            return Err(format!(
                "quantization_options levels must be between {MIN_SCALAR_LEVELS} and {MAX_SCALAR_LEVELS}: {levels}"
            ));
        }
        u16::try_from(levels)
            .map_err(|_| format!("quantization_options levels exceed u16: {levels}"))?
    } else {
        u16::try_from(MAX_SCALAR_LEVELS)
            .map_err(|_| "default scalar levels exceed u16".to_owned())?
    };

    let min = optional_finite_f32(options, "min")?;
    let max = optional_finite_f32(options, "max")?;
    if let (Some(min), Some(max)) = (min, max)
        && min >= max
    {
        return Err(format!(
            "quantization_options min must be less than max: {min} >= {max}"
        ));
    }
    let bounds = match (min, max) {
        (Some(minimum), Some(maximum)) => {
            Some(ScalarBounds::new(minimum, maximum).map_err(|error| error.to_string())?)
        }
        (None, None) => None,
        _ => {
            return Err(
                "quantization_options scalar min and max must be supplied together".to_owned(),
            );
        }
    };
    CodecSpec::scalar(levels, bounds).map_err(|error| error.to_string())
}

fn parse_product_quantization_options(
    options: &serde_json::Map<String, Value>,
) -> Result<CodecSpec, String> {
    let subvector_dimensions = if let Some(dimensions) = options.get("subvector_dimensions") {
        let dimensions = dimensions.as_u64().ok_or_else(|| {
            "quantization_options subvector_dimensions must be a positive integer".to_owned()
        })?;
        if dimensions == 0 {
            return Err("quantization_options subvector_dimensions must be positive: 0".to_owned());
        }
        Some(usize::try_from(dimensions).map_err(|_| {
            format!("quantization_options subvector_dimensions exceed usize: {dimensions}")
        })?)
    } else {
        None
    };
    match subvector_dimensions {
        Some(dimensions) => CodecSpec::product(
            dimensions,
            DEFAULT_PRODUCT_CENTROIDS,
            DEFAULT_PRODUCT_ITERATIONS,
        ),
        None => CodecSpec::product_auto(DEFAULT_PRODUCT_CENTROIDS, DEFAULT_PRODUCT_ITERATIONS),
    }
    .map_err(|error| error.to_string())
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the finite f64 value is checked again after conversion to the f32 codec domain"
)]
fn optional_finite_f32(
    options: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<Option<f32>, String> {
    let Some(value) = options.get(key) else {
        return Ok(None);
    };
    let value = value
        .as_f64()
        .ok_or_else(|| format!("quantization_options {key} must be a finite number"))?;
    if !value.is_finite() {
        return Err(format!(
            "quantization_options {key} must be a finite number"
        ));
    }
    let value = value as f32;
    if !value.is_finite() {
        return Err(format!(
            "quantization_options {key} exceeds the finite f32 codec domain"
        ));
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_empty_and_supported_quantization_metadata() -> Result<(), String> {
        for value in [
            json!({}),
            json!({"metadata_version": 1, "mode": "none"}),
            json!({"version": 1, "mode": "binary"}),
            json!({"mode": "scalar", "levels": 256, "min": -1.0, "max": 1.0}),
            json!({"mode": "sq8", "levels": 2}),
            json!({"mode": "pq", "subvector_dimensions": 8}),
        ] {
            validate_quantization_options(&value)
                .map_err(|error| format!("expected valid metadata {value}: {error}"))?;
        }
        Ok(())
    }

    #[test]
    fn aliases_and_build_options_resolve_to_one_typed_configuration() -> Result<(), String> {
        assert_eq!(
            parse_quantization_options(&json!({
                "mode": "sq8",
                "levels": 16,
                "min": -2.0,
                "max": 3.0
            }))?,
            CodecSpec::scalar(
                16,
                Some(ScalarBounds::new(-2.0, 3.0).map_err(|error| error.to_string())?),
            )
            .map_err(|error| error.to_string())?
        );
        assert_eq!(
            parse_quantization_options(&json!({
                "mode": "pq",
                "subvector_dimensions": 4
            }))?,
            CodecSpec::product(4, DEFAULT_PRODUCT_CENTROIDS, DEFAULT_PRODUCT_ITERATIONS)
                .map_err(|error| error.to_string())?
        );
        Ok(())
    }

    #[test]
    fn rejects_future_or_malformed_quantization_metadata() {
        let cases = [
            (
                json!({"metadata_version": 2}),
                "unsupported quantization_options metadata_version: 2",
            ),
            (
                json!({"metadata_version": "1"}),
                "quantization_options metadata_version must be a positive integer",
            ),
            (
                json!({"mode": "future"}),
                "unsupported quantization_options mode: future",
            ),
            (
                json!({"mode": "scalar", "levels": 1}),
                "quantization_options levels must be between 2 and 256: 1",
            ),
            (
                json!({"mode": "scalar", "min": 2.0, "max": 1.0}),
                "quantization_options min must be less than max: 2 >= 1",
            ),
            (
                json!({"mode": "pq", "subvector_dimensions": 0}),
                "quantization_options subvector_dimensions must be positive: 0",
            ),
            (
                json!({"mode": "scalar", "min": -1.0}),
                "quantization_options scalar min and max must be supplied together",
            ),
        ];

        for (value, expected) in cases {
            assert_eq!(
                validate_quantization_options(&value),
                Err(expected.to_owned())
            );
        }
    }
}
