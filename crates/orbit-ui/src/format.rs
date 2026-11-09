//! Number formatting shared by every panel, matching the Swift app's Foundation formatters in the
//! `en_US` locale:
//!
//! - [`format_bytes`]: `ByteCountFormatter.string(fromByteCount:countStyle: .file)` — decimal
//!   (1000) units, whole kilobytes, one fraction digit for megabytes, two from gigabytes up,
//!   trailing zeros trimmed, half-up rounding that carries into the next unit ("999.95 MB" is
//!   "1 GB"), "Zero KB", "1 byte" and "N bytes".
//! - [`format_percent`]: `Double.formatted(.percent.precision(.fractionLength(1)))` — the
//!   shortest decimal form of the fraction, rounded half-even to one place ("29.5%").
//! - [`format_count`]: `Int.formatted()` — grouped thousands ("12,345").
//!
//! The expected strings in the tests were produced by Foundation on macOS (`-AppleLocale en_US`).

/// Byte counts like Finder: `"78 GB"`, `"264.1 GB"`, `"102.66 GB"`, `"512 bytes"`, `"Zero KB"`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    match bytes {
        0 => return "Zero KB".into(),
        1 => return "1 byte".into(),
        2..1000 => return format!("{bytes} bytes"),
        _ => {}
    }
    // Fraction digits per unit: KB 0, MB 1, GB and up 2.
    let digits = |unit: usize| match unit {
        0 => 0,
        1 => 1,
        _ => 2,
    };
    let mut unit = 0;
    let mut divisor: u128 = 1000;
    loop {
        let scale = 10u128.pow(digits(unit));
        // Half-up rounding in exact integer arithmetic.
        let scaled = (u128::from(bytes) * scale * 2 + divisor) / (divisor * 2);
        if scaled >= 1000 * scale && unit + 1 < UNITS.len() {
            unit += 1;
            divisor *= 1000;
            continue;
        }
        let (whole, mut fraction) = (scaled / scale, scaled % scale);
        let mut width = digits(unit) as usize;
        while width > 0 && fraction % 10 == 0 {
            fraction /= 10;
            width -= 1;
        }
        return if width == 0 {
            format!("{whole} {}", UNITS[unit])
        } else {
            format!("{whole}.{fraction:0width$} {}", UNITS[unit])
        };
    }
}

/// A share of a whole (`0.295` → `"29.5%"`), one fraction digit.
pub fn format_percent(fraction: f64) -> String {
    if !fraction.is_finite() {
        return "0.0%".into();
    }
    if fraction.abs() >= 1e12 {
        return format!("{:.1}%", fraction * 100.0);
    }
    // Foundation rounds the shortest round-trip decimal form of the value (not its binary
    // expansion), half to even: 0.0125 → "1.2%", 0.0135 → "1.4%".
    let negative = fraction < 0.0;
    let text = format!("{:e}", fraction.abs());
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    // `digits` = d0.d1d2… × 10^exponent; the percent value is that × 100. Keep tenths of a
    // percent: the digit with weight 10^-1 in percent units, i.e. 10^-3 of the fraction.
    let kept = (exponent + 3 + 1).max(0) as usize; // digits at or above 10^-3
    let mut tenths: u128 = 0;
    for &d in digits.iter().take(kept) {
        tenths = tenths * 10 + u128::from(d);
    }
    for _ in digits.len()..kept {
        tenths *= 10;
    }
    let rest = if kept < digits.len() { &digits[kept..] } else { &[][..] };
    // Below 10^-4 the first dropped digit is an implied zero.
    let round_up = exponent + 4 >= 0
        && match rest.split_first() {
            Some((&first, _)) if first > 5 => true,
            Some((&5, tail)) => tail.iter().any(|&d| d > 0) || tenths % 2 == 1,
            _ => false,
        };
    if round_up {
        tenths += 1;
    }
    let sign = if negative && tenths > 0 { "-" } else { "" };
    format!("{sign}{}.{}%", tenths / 10, tenths % 10)
}

/// `12,345` (grouped thousands).
pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ByteCountFormatter.string(fromByteCount:countStyle: .file)`, en_US (macOS 27).
    const BYTES: &[(u64, &str)] = &[
        (0, "Zero KB"),
        (1, "1 byte"),
        (2, "2 bytes"),
        (10, "10 bytes"),
        (500, "500 bytes"),
        (999, "999 bytes"),
        (1000, "1 KB"),
        (1001, "1 KB"),
        (1023, "1 KB"),
        (1024, "1 KB"),
        (1499, "1 KB"),
        (1500, "2 KB"),
        (1501, "2 KB"),
        (2047, "2 KB"),
        (9999, "10 KB"),
        (10000, "10 KB"),
        (12345, "12 KB"),
        (45000, "45 KB"),
        (99499, "99 KB"),
        (99500, "100 KB"),
        (99999, "100 KB"),
        (100000, "100 KB"),
        (123456, "123 KB"),
        (999499, "999 KB"),
        (999500, "1 MB"),
        (999999, "1 MB"),
        (1000000, "1 MB"),
        (1049999, "1 MB"),
        (1050000, "1.1 MB"),
        (1234567, "1.2 MB"),
        (1250000, "1.3 MB"),
        (1500000, "1.5 MB"),
        (1999500, "2 MB"),
        (2345000, "2.3 MB"),
        (2500000, "2.5 MB"),
        (9949999, "9.9 MB"),
        (9950000, "10 MB"),
        (9999999, "10 MB"),
        (12345678, "12.3 MB"),
        (99949999, "99.9 MB"),
        (99950000, "100 MB"),
        (123456789, "123.5 MB"),
        (450000000, "450 MB"),
        (500000000, "500 MB"),
        (999949999, "999.9 MB"),
        (999950000, "1 GB"),
        (999999999, "1 GB"),
        (1000000000, "1 GB"),
        (1004999999, "1 GB"),
        (1005000000, "1.01 GB"),
        (1234567890, "1.23 GB"),
        (1250000000, "1.25 GB"),
        (1255000000, "1.26 GB"),
        (1265000000, "1.27 GB"),
        (9995000000, "10 GB"),
        (12345678901, "12.35 GB"),
        (25000000000, "25 GB"),
        (78000000000, "78 GB"),
        (99995000000, "100 GB"),
        (102660000000, "102.66 GB"),
        (102664999999, "102.66 GB"),
        (123456789012, "123.46 GB"),
        (264096000000, "264.1 GB"),
        (264100000000, "264.1 GB"),
        (342000000000, "342 GB"),
        (999994999999, "999.99 GB"),
        (999995000000, "1 TB"),
        (1000000000000, "1 TB"),
        (1234567890123, "1.23 TB"),
        (12345678901234, "12.35 TB"),
        (999999999999999, "1 PB"),
        (1000000000000000, "1 PB"),
        (1234567890123456, "1.23 PB"),
        (1234567890123456789, "1.23 EB"),
        (9223372036854775807, "9.22 EB"),
    ];

    #[test]
    fn bytes_match_foundation() {
        for &(bytes, expected) in BYTES {
            assert_eq!(format_bytes(bytes), expected, "{bytes} bytes");
        }
        // Beyond Int64 (Foundation's range) the largest unit keeps growing.
        assert_eq!(format_bytes(u64::MAX), "18.45 EB");
    }

    /// `Double.formatted(.percent.precision(.fractionLength(1)))`, en_US.
    #[test]
    fn percents_match_foundation() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0%"),
            (0.0004, "0.0%"),
            (0.00049, "0.0%"),
            (0.0005, "0.0%"),
            (0.001, "0.1%"),
            (0.0123, "1.2%"),
            (0.0125, "1.2%"),
            (0.0135, "1.4%"),
            (0.12345, "12.3%"),
            (0.29546, "29.5%"),
            (0.2955, "29.6%"),
            (1.0, "100.0%"),
            (0.0000006, "0.0%"),
        ];
        for &(fraction, expected) in cases {
            assert_eq!(format_percent(fraction), expected, "{fraction}");
        }
        assert_eq!(format_percent(78.0 / 264.1), "29.5%");
        assert_eq!(format_percent(f64::NAN), "0.0%");
    }

    #[test]
    fn counts_group_thousands() {
        for (count, expected) in
            [(0, "0"), (1, "1"), (999, "999"), (1000, "1,000"), (12345, "12,345"), (1234567, "1,234,567")]
        {
            assert_eq!(format_count(count), expected);
        }
    }
}
