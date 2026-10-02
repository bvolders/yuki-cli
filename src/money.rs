//! Exact money and decimal arithmetic: amounts in whole cents, and decimal
//! text parsed into scaled integers, so a typed amount never passes through
//! `f64`.

/// An amount in whole cents, so amounts compare exactly and key maps directly.
///
/// Assumes the API sends at most two decimals, which Yuki does. An amount
/// with more is rounded per amount, half away from zero (`1.005` may land on
/// either cent through its `f64` form), where the code before `Cents` compared
/// `{:.2}`-formatted strings; the two can differ only on such amounts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cents(pub i64);

impl Cents {
    pub const ZERO: Self = Self(0);

    /// Parse an API amount such as `"-7.3"`, rounded to the cent.
    pub fn parse(amount: &str) -> Option<Self> {
        let value = amount
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|a| a.is_finite())?;
        Some(Self((value * 100.0).round() as i64))
    }

    /// Parse a typed amount with at most two decimals, exactly: `"1250.5"` is
    /// 125050 cents, and `"0.005"` is an error rather than a rounded cent.
    pub fn parse_exact(text: &str) -> Result<Self, String> {
        parse_scaled(text, 2).map(Self)
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }
}

impl std::ops::Neg for Cents {
    type Output = Self;
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl std::ops::Add for Cents {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl std::iter::Sum for Cents {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        Self(iter.map(|c| c.0).sum())
    }
}

impl Cents {
    /// Belgian notation: a dot between thousands, a comma before the cents,
    /// as an invoice text writes amounts: `4.312,50`.
    pub fn belgian(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let digits = (abs / 100).to_string();
        let mut grouped = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push('.');
            }
            grouped.push(c);
        }
        format!("{sign}{grouped},{:02}", abs % 100)
    }
}

impl std::fmt::Display for Cents {
    /// Two decimals with a dot, as the API writes amounts: `-7.30`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
    }
}

/// Parse decimal text such as `"-7.5"` into an integer scaled by
/// `10^decimals` (`"-7.5"`, 2 → -750). More decimals than `decimals`, an
/// exponent, a comma, or anything but digits, one dot and a leading sign is
/// an error.
pub fn parse_scaled(text: &str, decimals: u32) -> Result<i64, String> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let is_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !is_digits(whole) || !is_digits(fraction) {
        return Err(format!("'{text}' is not a decimal number"));
    }
    if fraction.len() > decimals as usize {
        return Err(format!("'{text}' has more than {decimals} decimals"));
    }
    let padded = format!("{whole}{fraction:0<width$}", width = decimals as usize);
    let value: i64 = padded
        .parse()
        .map_err(|_| format!("'{text}' is out of range"))?;
    Ok(if negative { -value } else { value })
}

/// Format a value scaled by `10^decimals` with a dot and no trailing zeros:
/// 75000, 4 → `"7.5"`; 2100, 2 → `"21"`.
pub fn format_scaled(value: i64, decimals: u32) -> String {
    let scale = 10_u64.pow(decimals);
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let fraction = format!("{:0width$}", abs % scale, width = decimals as usize);
    match fraction.trim_end_matches('0') {
        "" => format!("{sign}{}", abs / scale),
        fraction => format!("{sign}{}.{fraction}", abs / scale),
    }
}

/// `numerator / denominator` for a positive `denominator`, rounded half away
/// from zero.
pub fn div_round(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0, "denominator must be positive");
    let half = denominator / 2;
    if numerator >= 0 {
        (numerator + half) / denominator
    } else {
        -((half - numerator) / denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn belgian_notation_groups_thousands_with_dots() {
        assert_eq!(Cents(431_250).belgian(), "4.312,50");
        assert_eq!(Cents(123_456_789).belgian(), "1.234.567,89");
        assert_eq!(Cents(5).belgian(), "0,05");
        assert_eq!(Cents(-100_000).belgian(), "-1.000,00");
    }

    #[test]
    fn cents_parse_and_print_like_the_api() {
        assert_eq!(Cents::parse(" -7.3 "), Some(Cents(-730)));
        assert_eq!(Cents::parse("133.20"), Some(Cents(13320)));
        assert_eq!(Cents::parse("x"), None);
        assert_eq!(Cents(-730).to_string(), "-7.30");
        assert_eq!(Cents(5).to_string(), "0.05");
        assert_eq!(Cents(-5).to_string(), "-0.05");
    }

    #[test]
    fn exact_parsing_rejects_what_it_would_have_to_round() {
        assert_eq!(Cents::parse_exact("1250.5"), Ok(Cents(125_050)));
        assert_eq!(Cents::parse_exact("-0.05"), Ok(Cents(-5)));
        assert_eq!(Cents::parse_exact("100"), Ok(Cents(10_000)));
        assert!(Cents::parse_exact("0.005").is_err());
        for bad in ["", ".", "1,5", "1e3", "--1", "1.2.3", "- 1", "abc"] {
            assert!(parse_scaled(bad, 2).is_err(), "{bad:?}");
        }
        assert_eq!(parse_scaled("7.5", 4), Ok(75_000));
        assert_eq!(parse_scaled(".5", 2), Ok(50));
    }

    #[test]
    fn scaled_values_print_without_trailing_zeros() {
        assert_eq!(format_scaled(75_000, 4), "7.5");
        assert_eq!(format_scaled(10_000, 4), "1");
        assert_eq!(format_scaled(2_100, 2), "21");
        assert_eq!(format_scaled(-550, 2), "-5.5");
        assert_eq!(format_scaled(5, 4), "0.0005");
    }

    #[test]
    fn division_rounds_half_away_from_zero() {
        assert_eq!(div_round(5, 10), 1);
        assert_eq!(div_round(-5, 10), -1);
        assert_eq!(div_round(4, 10), 0);
        assert_eq!(div_round(-14, 10), -1);
        assert_eq!(div_round(-15, 10), -2);
    }
}
