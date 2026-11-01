//! Human size parsing and formatting. `gb` is decimal, `gib` is binary, matching
//! how Finder and Explorer respectively tend to show them.

/// Parses sizes like `1gb`, `500 MiB`, `1.5g`, or a bare byte count.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let text = input.trim().to_ascii_lowercase();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let multiplier: u64 = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        other => return Err(format!("unknown size unit `{other}` in `{input}`")),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("`{input}` is not a size, expected something like 1gb or 500mib"))?;
    let bytes = value * multiplier as f64;
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err(format!("`{input}` is out of range"));
    }
    Ok(bytes as u64)
}

/// Formats bytes with decimal units, to one decimal place.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_and_binary_units() {
        assert_eq!(parse_size("1gb"), Ok(1_000_000_000));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("1.5 mb"), Ok(1_500_000));
        assert_eq!(parse_size("4096"), Ok(4096));
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_size("lots").is_err());
        assert!(parse_size("1zb").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("99999999999tb").is_err());
    }

    #[test]
    fn formats_decimal_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2_400_000_000), "2.4 GB");
    }
}
