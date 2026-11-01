//! Durations on the command line, like `--stale 30d`. Hours and up only: `m` could
//! mean minutes or months, and nothing here needs that precision.

/// Parses `12h`, `30d`, or `2w` into seconds.
pub fn parse_age(input: &str) -> Result<u64, String> {
    let text = input.trim().to_ascii_lowercase();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let seconds_per_unit: u64 = match unit {
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return Err(format!("`{input}` is not an age, expected 12h, 30d, or 2w")),
    };
    let value: u64 = number
        .parse()
        .map_err(|_| format!("`{input}` is not an age, expected 12h, 30d, or 2w"))?;
    value
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| format!("`{input}` is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hours_days_and_weeks() {
        assert_eq!(parse_age("12h"), Ok(43_200));
        assert_eq!(parse_age("30D"), Ok(2_592_000));
        assert_eq!(parse_age("2w"), Ok(1_209_600));
    }

    #[test]
    fn rejects_nonsense() {
        for input in ["", "30", "d", "5m", "-1d", "1.5d", "99999999999999999999w"] {
            assert!(parse_age(input).is_err(), "{input}");
        }
    }
}
