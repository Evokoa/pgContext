//! Validation for SQL-visible vector metadata JSON.

use serde_json::Value;

const CURRENT_QUANTIZATION_METADATA_VERSION: u64 = 1;
const MIN_SCALAR_LEVELS: u64 = 2;
const MAX_SCALAR_LEVELS: u64 = 256;

pub(crate) fn validate_quantization_options(value: &Value) -> Result<(), String> {
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

    let Some(mode) = options.get("mode") else {
        return Ok(());
    };
    let mode = mode
        .as_str()
        .ok_or_else(|| "quantization_options mode must be a string".to_owned())?;
    match mode {
        "none" | "binary" => Ok(()),
        "scalar" | "sq8" => validate_scalar_quantization_options(options),
        "pq" => validate_product_quantization_options(options),
        _ => Err(format!("unsupported quantization_options mode: {mode}")),
    }
}

fn validate_scalar_quantization_options(
    options: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    if let Some(levels) = options.get("levels") {
        let levels = levels
            .as_u64()
            .ok_or_else(|| "quantization_options levels must be a positive integer".to_owned())?;
        if !(MIN_SCALAR_LEVELS..=MAX_SCALAR_LEVELS).contains(&levels) {
            return Err(format!(
                "quantization_options levels must be between {MIN_SCALAR_LEVELS} and {MAX_SCALAR_LEVELS}: {levels}"
            ));
        }
    }

    let min = optional_finite_number(options, "min")?;
    let max = optional_finite_number(options, "max")?;
    if let (Some(min), Some(max)) = (min, max)
        && min >= max
    {
        return Err(format!(
            "quantization_options min must be less than max: {min} >= {max}"
        ));
    }
    Ok(())
}

fn validate_product_quantization_options(
    options: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    if let Some(dimensions) = options.get("subvector_dimensions") {
        let dimensions = dimensions.as_u64().ok_or_else(|| {
            "quantization_options subvector_dimensions must be a positive integer".to_owned()
        })?;
        if dimensions == 0 {
            return Err("quantization_options subvector_dimensions must be positive: 0".to_owned());
        }
    }
    Ok(())
}

fn optional_finite_number(
    options: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<Option<f64>, String> {
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
        ];

        for (value, expected) in cases {
            assert_eq!(
                validate_quantization_options(&value),
                Err(expected.to_owned())
            );
        }
    }
}
