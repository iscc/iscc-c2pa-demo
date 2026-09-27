//! Excel number formats, rendered the way Tika (Apache POI's `DataFormatter`, English locale)
//! shows spreadsheet cells. The Content-Code Text drops whitespace and punctuation but keeps
//! letters, digits and symbols such as `+`, so what matters is which of those appear: General,
//! fixed decimals, percentages, scientific notation, dates and times are rendered; grouping
//! separators and minus signs may differ freely. Fractions and anything not understood keep
//! the stored value. Numbers are rounded half up on Excel's 15 significant digits, as POI does.
//! Known deviations, rare enough to leave: engineering notation (`##0.0E+0`, exponents in
//! steps of three) is written as plain scientific notation, and General values below 1E-10
//! can round differently at the tenth decimal (Tika shows 5E-11 as 0, a quirk of Java's
//! `DecimalFormat`).

/// English month names, for `mmm` and `mmmm`.
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// English day names from Sunday, for `ddd` and `dddd`.
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
/// Days from 1970-01-01 back to the day before serial 1 in the 1900 system (1899-12-30, which
/// accounts for Excel's fictitious 1900-02-29) and to serial 0 in the 1904 system.
const EPOCH_1900: i64 = -25569;
const EPOCH_1904: i64 = -24107;

/// Format code of a built-in number format id, as POI lists them.
pub fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        14 => "m/d/yy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yy h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// Display text of the stored numeric value `raw` under the format `code`.
pub fn format(raw: &str, code: &str, date1904: bool) -> String {
    let Ok(value) = raw.trim().parse::<f64>() else {
        return raw.to_owned();
    };
    let section = section(code, value);
    let plain = strip_literals(&section);
    if plain.eq_ignore_ascii_case("general") || plain.trim() == "@" {
        return general(value);
    }
    if is_date(&plain) {
        return date(value, &section, date1904);
    }
    if plain.contains('/') {
        // Fractions.
        return raw.to_owned();
    }
    number(value.abs(), &section, &plain).unwrap_or_else(|| raw.to_owned())
}

/// The format section that applies: positive; negative; zero.
fn section(code: &str, value: f64) -> String {
    let sections = split_sections(code);
    let index = match sections.len() {
        n if value < 0.0 && n > 1 => 1,
        n if value == 0.0 && n > 2 => 2,
        _ => 0,
    };
    sections[index].clone()
}

/// Split a format code at the `;` that are not quoted or escaped.
fn split_sections(code: &str) -> Vec<String> {
    let mut sections = vec![String::new()];
    let (mut quoted, mut escaped) = (false, false);
    for c in code.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                sections.push(String::new());
                continue;
            }
            _ => {}
        }
        sections.last_mut().expect("one section").push(c);
    }
    sections
}

/// The section without quoted text, escaped characters, `[...]` modifiers other than elapsed
/// time, and `_x` / `*x` padding: what decides the kind of format.
fn strip_literals(section: &str) -> String {
    let mut out = String::new();
    let mut chars = section.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => chars.by_ref().take_while(|&c| c != '"').for_each(drop),
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let inner: String = chars.by_ref().take_while(|&c| c != ']').collect();
                if is_elapsed(&inner) {
                    out.push_str(&inner);
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `[h]`, `[mm]`, `[ss]`: elapsed time.
fn is_elapsed(inner: &str) -> bool {
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| matches!(c, 'h' | 'H' | 'm' | 'M' | 's' | 'S'))
}

/// Whether a stripped section formats dates or times.
fn is_date(plain: &str) -> bool {
    plain
        .chars()
        .any(|c| matches!(c.to_ascii_lowercase(), 'y' | 'm' | 'd' | 'h' | 's'))
}

/// Tika's General format: Excel's 15 significant digits, scientific notation above 1E15 and at
/// or below 1E-15, else at most 10 decimals. Java's `DecimalFormat` shows no more than the
/// shortest digits of the value and rounds longer ones half up on the binary value.
fn general(value: f64) -> String {
    let magnitude = Digits::excel(value).value();
    let text = if magnitude > 1e15 || (magnitude > 0.0 && magnitude <= 1e-15) {
        scientific(value, &GENERAL_MANTISSA, 1)
    } else {
        let shortest = Digits::shortest(magnitude);
        let decimals = shortest.digits.len() as i32 - shortest.exponent - 1;
        let digits = if decimals > GENERAL_DECIMALS.fraction_max as i32 {
            Digits::exact(magnitude)
        } else {
            shortest
        };
        fixed(&digits, &GENERAL_DECIMALS)
    };
    if value < 0.0 {
        format!("-{text}")
    } else {
        text
    }
}

/// A plain number: fixed decimals, percentage or scientific notation, between the literal text
/// before and after its placeholders.
fn number(value: f64, section: &str, plain: &str) -> Option<String> {
    let mantissa_end = plain.find(['E', 'e']).unwrap_or(plain.len());
    let placeholders = Placeholders::parse(&plain[..mantissa_end]);
    let rendered = if mantissa_end < plain.len() {
        let digits = plain[mantissa_end..]
            .chars()
            .filter(|c| matches!(c, '0' | '#'))
            .count();
        scientific(value, &placeholders, digits)
    } else if plain.contains('%') {
        format!("{}%", fixed(&Digits::excel(value).scaled(2), &placeholders))
    } else if plain.contains(['0', '#']) {
        fixed(&Digits::excel(value), &placeholders)
    } else {
        return None;
    };
    let (start, end) = placeholder_span(section)?;
    Some(format!(
        "{}{rendered}{}",
        literal_text(&section[..start]),
        literal_text(&section[end..])
    ))
}

/// Byte range from the first to past the last unquoted digit placeholder of a section.
fn placeholder_span(section: &str) -> Option<(usize, usize)> {
    let (mut quoted, mut escaped) = (false, false);
    let mut span: Option<(usize, usize)> = None;
    for (i, c) in section.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            '0' | '#' if !quoted => {
                span = Some((span.map_or(i, |(s, _)| s), i + 1));
            }
            _ => {}
        }
    }
    span
}

/// Digit placeholders of a number's mantissa: `0` always shows a digit, `#` only a significant
/// one. POI passes Excel's `?` to Java's `DecimalFormat`, which prints it as literal text.
struct Placeholders {
    integer_min: usize,
    fraction_min: usize,
    fraction_max: usize,
}

/// General's plain numbers (`#.##########`) and scientific mantissa (`0.##############`).
const GENERAL_DECIMALS: Placeholders = Placeholders {
    integer_min: 1,
    fraction_min: 0,
    fraction_max: 10,
};
const GENERAL_MANTISSA: Placeholders = Placeholders {
    integer_min: 1,
    fraction_min: 0,
    fraction_max: 14,
};

impl Placeholders {
    /// Count the placeholders of a mantissa such as `#,##0.00`.
    fn parse(mantissa: &str) -> Self {
        let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let zeros = |s: &str| s.chars().filter(|&c| c == '0').count();
        // Java's DecimalFormat, which POI formats with, reads `#.##` as `0.##`.
        let integer_min =
            if !mantissa.contains('0') && integer.contains('#') && !fraction.is_empty() {
                1
            } else {
                zeros(integer)
            };
        Self {
            integer_min,
            fraction_min: zeros(fraction),
            fraction_max: fraction.chars().filter(|c| matches!(c, '0' | '#')).count(),
        }
    }
}

/// A magnitude as decimal digits and the power of ten of the first digit; no digits is zero.
#[derive(Clone)]
struct Digits {
    digits: Vec<u8>,
    exponent: i32,
}

impl Digits {
    /// The exact decimal expansion of `value`'s magnitude, to 40 significant digits.
    fn exact(value: f64) -> Self {
        Self::parse(&format!("{:.39e}", value.abs()))
    }

    /// The fewest decimal digits that read back as `value`'s magnitude.
    fn shortest(value: f64) -> Self {
        Self::parse(&format!("{:e}", value.abs()))
    }

    /// Read Rust's `{:e}` notation of a non-negative number.
    fn parse(text: &str) -> Self {
        let (mantissa, exponent) = text.split_once('e').expect("scientific notation");
        Self {
            digits: mantissa
                .bytes()
                .filter(u8::is_ascii_digit)
                .map(|b| b - b'0')
                .collect(),
            exponent: exponent.parse().expect("integer exponent"),
        }
    }

    /// The value POI formats: Excel's 15 significant digits (`NumberToTextConverter`).
    fn excel(value: f64) -> Self {
        Self::exact(value).round(15)
    }

    /// Rounded half up to the first `keep` digits; the exponent grows when rounding carries.
    fn round(&self, keep: i32) -> Self {
        let Ok(keep) = usize::try_from(keep + 1) else {
            return Self {
                digits: Vec::new(),
                exponent: 0,
            };
        };
        // A leading zero takes the carry of 9.99 rounding up to 10.0.
        let mut digits: Vec<u8> = std::iter::once(0)
            .chain(self.digits.iter().copied())
            .collect();
        let up = digits.get(keep).is_some_and(|&d| d >= 5);
        digits.truncate(keep);
        if up {
            for d in digits.iter_mut().rev() {
                *d = (*d + 1) % 10;
                if *d != 0 {
                    break;
                }
            }
        }
        match digits.first() {
            Some(0) => Self {
                digits: digits.split_off(1),
                exponent: self.exponent,
            },
            _ => Self {
                digits,
                exponent: self.exponent + 1,
            },
        }
    }

    /// The same digits times ten to the `power`.
    fn scaled(self, power: i32) -> Self {
        Self {
            exponent: self.exponent + power,
            ..self
        }
    }

    /// The digit at the power of ten `power`.
    fn digit(&self, power: i32) -> char {
        let digit = usize::try_from(self.exponent - power)
            .ok()
            .and_then(|i| self.digits.get(i))
            .map_or(0, |&d| d);
        char::from(b'0' + digit)
    }

    /// The number as a float.
    fn value(&self) -> f64 {
        let digits: String = self.digits.iter().map(|&d| char::from(b'0' + d)).collect();
        format!("0.{digits}e{}", self.exponent + 1)
            .parse()
            .unwrap_or(0.0)
    }
}

/// Write `digits` with the placeholders: rounded half up to the most fraction digits, trailing
/// fraction zeros dropped down to the fewest, the integer part padded with zeros, and a lone
/// `0` when nothing else shows.
fn fixed(digits: &Digits, placeholders: &Placeholders) -> String {
    let rounded = digits.round(digits.exponent + 1 + placeholders.fraction_max as i32);
    let integer: String = (0..=rounded.exponent)
        .rev()
        .map(|p| rounded.digit(p))
        .collect();
    let integer = format!(
        "{:0>width$}",
        integer.trim_start_matches('0'),
        width = placeholders.integer_min
    );
    let fraction: String = (1..=placeholders.fraction_max as i32)
        .map(|p| rounded.digit(-p))
        .collect();
    let keep = fraction
        .trim_end_matches('0')
        .len()
        .max(placeholders.fraction_min);
    match (integer.is_empty(), keep) {
        (true, 0) => "0".to_owned(),
        (_, 0) => integer,
        _ => format!("{integer}.{}", &fraction[..keep]),
    }
}

/// `1.23E+04` style: the mantissa with the placeholders, the exponent with at least `digits`
/// digits; a mantissa that rounds up to 10 moves to the next exponent.
fn scientific(value: f64, placeholders: &Placeholders, digits: usize) -> String {
    let number = Digits::excel(value);
    let exponent = if number.digits.iter().all(|&d| d == 0) {
        0
    } else {
        number.exponent
    };
    let rounded = number
        .scaled(-exponent)
        .round(1 + placeholders.fraction_max as i32);
    // 0, or 1 when the mantissa rounded up to 10.
    let carry = rounded.exponent;
    let mantissa = fixed(&rounded.scaled(-carry), placeholders);
    let exponent = exponent + carry;
    let sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{mantissa}E{sign}{:0digits$}",
        exponent.abs(),
        digits = digits.max(1)
    )
}

/// Quoted and escaped text of a section, which the number is shown with.
fn literal_text(section: &str) -> String {
    let mut out = String::new();
    let mut chars = section.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.extend(chars.by_ref().take_while(|&c| c != '"')),
            '\\' => out.extend(chars.next()),
            _ => {}
        }
    }
    out
}

/// One piece of a date or time format.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Year(usize),
    /// `m` run: a month, or minutes next to hours or seconds.
    M(usize),
    Minute(usize),
    Day(usize),
    Hour(usize),
    Second(usize),
    /// Fractional seconds (`.0`, `.00`, `.000`).
    Fraction(usize),
    /// `[h]`, `[mm]`, `[s]`: total elapsed units, and the width of the run.
    Elapsed(char, usize),
    AmPm,
    Text(String),
}

/// Split a date section into tokens and tell minutes from months.
fn tokens(section: &str) -> Vec<Token> {
    let chars: Vec<char> = section.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (token, used) = token_at(&chars[i..]);
        out.extend(token);
        i += used.max(1);
    }
    resolve_minutes(out)
}

/// The token at the start of `chars` and how many characters it used.
fn token_at(chars: &[char]) -> (Option<Token>, usize) {
    let c = chars[0];
    let run = chars
        .iter()
        .take_while(|x| x.eq_ignore_ascii_case(&c))
        .count();
    let rest: String = chars.iter().collect();
    match c.to_ascii_lowercase() {
        'y' => (Some(Token::Year(run)), run),
        'm' => (Some(Token::M(run)), run),
        'd' => (Some(Token::Day(run)), run),
        'h' => (Some(Token::Hour(run)), run),
        's' => (Some(Token::Second(run)), run),
        'a' if rest.to_ascii_uppercase().starts_with("AM/PM") => (Some(Token::AmPm), 5),
        'a' if rest.to_ascii_uppercase().starts_with("A/P") => (Some(Token::AmPm), 3),
        '.' if chars.get(1) == Some(&'0') => {
            let zeros = chars[1..].iter().take_while(|&&x| x == '0').count();
            (Some(Token::Fraction(zeros)), zeros + 1)
        }
        _ => literal_token(chars),
    }
}

/// Quoted text, an escaped character, a bracket modifier, padding or a plain character.
fn literal_token(chars: &[char]) -> (Option<Token>, usize) {
    let text = |s: String| Some(Token::Text(s));
    match chars[0] {
        '"' => {
            let inner: String = chars[1..].iter().take_while(|&&c| c != '"').collect();
            let used = inner.chars().count() + 2;
            (text(inner), used)
        }
        '\\' => (
            text(chars.get(1).map(|c| c.to_string()).unwrap_or_default()),
            2,
        ),
        '_' | '*' => (None, 2),
        '[' => {
            let inner: String = chars[1..].iter().take_while(|&&c| c != ']').collect();
            let used = inner.chars().count() + 2;
            let unit = inner.chars().next().unwrap_or('h').to_ascii_lowercase();
            let elapsed = is_elapsed(&inner).then(|| Token::Elapsed(unit, inner.chars().count()));
            (elapsed, used)
        }
        c => (text(c.to_string()), 1),
    }
}

/// `m` and `mm` are minutes right after an hour or right before a second, months otherwise.
fn resolve_minutes(tokens: Vec<Token>) -> Vec<Token> {
    let is_unit = |t: &Token| !matches!(t, Token::Text(_));
    let units: Vec<usize> = (0..tokens.len()).filter(|&i| is_unit(&tokens[i])).collect();
    let mut out = tokens.clone();
    for (k, &i) in units.iter().enumerate() {
        let Token::M(n) = tokens[i] else { continue };
        let after_hour = k > 0
            && matches!(
                tokens[units[k - 1]],
                Token::Hour(_) | Token::Elapsed('h', _)
            );
        let before_second = units
            .get(k + 1)
            .is_some_and(|&j| matches!(tokens[j], Token::Second(_) | Token::Elapsed('s', _)));
        if n <= 2 && (after_hour || before_second) {
            out[i] = Token::Minute(n);
        }
    }
    out
}

/// Calendar date and time of a serial date value.
struct DateTime {
    year: i64,
    month: usize,
    day: i64,
    weekday: usize,
    /// Milliseconds into the day.
    millis: i64,
    /// The serial value, for elapsed time and fractional seconds.
    serial: f64,
}

/// Render a date or time format.
fn date(value: f64, section: &str, date1904: bool) -> String {
    let tokens = tokens(section);
    let dt = date_time(value, date1904);
    let twelve_hour = tokens.contains(&Token::AmPm);
    tokens.iter().map(|t| render(t, &dt, twelve_hour)).collect()
}

/// Split a serial value into calendar date and time of day as POI's `DateUtil` does: the time
/// is rounded to the millisecond, which can carry into the next day, and shown cut to the
/// second.
fn date_time(serial: f64, date1904: bool) -> DateTime {
    let whole = serial.floor();
    let millis = ((serial - whole) * 86_400_000.0 + 0.5) as i64;
    let epoch = if date1904 { EPOCH_1904 } else { EPOCH_1900 };
    // Serials before 1900-03-01 count Excel's fictitious 1900-02-29.
    let shift = if !date1904 && whole < 61.0 { 1 } else { 0 };
    let days = epoch + whole as i64 + shift + millis / 86_400_000;
    let (year, month, day) = civil(days);
    DateTime {
        year,
        month,
        day,
        weekday: (days + 4).rem_euclid(7) as usize,
        millis: millis % 86_400_000,
        serial,
    }
}

/// Year, month (1-12) and day of the day `days` after 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month as usize, day)
}

/// Text of one date token.
fn render(token: &Token, dt: &DateTime, twelve_hour: bool) -> String {
    let pad = |v: i64, n: usize| format!("{v:0width$}", width = n.min(2));
    let seconds = dt.millis / 1000;
    let hour = seconds / 3600;
    match token {
        Token::Year(n) if *n <= 2 => format!("{:02}", dt.year.rem_euclid(100)),
        Token::Year(_) => dt.year.to_string(),
        Token::M(n) => month_text(dt.month, *n),
        Token::Minute(n) => pad(seconds / 60 % 60, *n),
        Token::Day(n) if *n <= 2 => pad(dt.day, *n),
        Token::Day(n) => day_text(dt.weekday, *n),
        Token::Hour(n) if twelve_hour => pad((hour + 11) % 12 + 1, *n),
        Token::Hour(n) => pad(hour, *n),
        Token::Second(n) => pad(seconds % 60, *n),
        Token::Fraction(n) => fraction(dt, *n),
        Token::Elapsed(unit, n) => pad(elapsed(dt.serial, *unit), *n),
        Token::AmPm => if hour < 12 { "AM" } else { "PM" }.to_owned(),
        Token::Text(text) => text.clone(),
    }
}

/// Month as a number (`m`, `mm`), abbreviation (`mmm`), name (`mmmm`) or initial (`mmmmm`).
fn month_text(month: usize, n: usize) -> String {
    let name = MONTHS[month - 1];
    match n {
        1 => month.to_string(),
        2 => format!("{month:02}"),
        3 => name[..3].to_owned(),
        4 => name.to_owned(),
        _ => name[..1].to_owned(),
    }
}

/// Weekday as an abbreviation (`ddd`) or name (`dddd`).
fn day_text(weekday: usize, n: usize) -> String {
    let name = DAYS[weekday];
    if n == 3 {
        name[..3].to_owned()
    } else {
        name.to_owned()
    }
}

/// Fractional seconds as POI's `ExcelStyleDateFormatter` writes them: `.0` and `.00` from the
/// time of day in single precision, rounded half up; `.000` as the milliseconds.
fn fraction(dt: &DateTime, n: usize) -> String {
    if n >= 3 {
        return format!(".{:03}", dt.millis % 1000);
    }
    let seconds = ((dt.serial - dt.serial.floor()) * 86_400.0) as f32;
    let part = f64::from(seconds - seconds.trunc());
    format!(".{:0n$}", (part * 10f64.powi(n as i32)).round() as i64)
}

/// Total elapsed hours, minutes or seconds of a serial value, computed in single precision and
/// cut down, as POI's `ExcelStyleDateFormatter` does.
fn elapsed(serial: f64, unit: char) -> i64 {
    let total = match unit {
        'h' => serial as f32 * 24.0,
        'm' => serial as f32 * 24.0 * 60.0,
        _ => (serial * 24.0 * 60.0 * 60.0) as f32,
    };
    total.floor() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_percentages_and_scientific() {
        assert_eq!(format("42", "General", false), "42");
        assert_eq!(format("3.25", "0.0", false), "3.3");
        assert_eq!(format("1234.5", "#,##0.00", false), "1234.50");
        assert_eq!(format("0.5", "0%", false), "50%");
        assert_eq!(format("0.1234", "0.00%", false), "12.34%");
        assert_eq!(format("12345", "0.00E+00", false), "1.23E+04");
        assert_eq!(format("-7", "0;(0)", false), "7");
        assert_eq!(format("5", "0 \"pcs\"", false), "5pcs");
        assert_eq!(format("5", "\"No. \"0", false), "No. 5");
        assert_eq!(format("0.75", "# ?/?", false), "0.75");
        assert_eq!(format("abc", "0.00", false), "abc");
    }

    // The expected strings in the tests below are what Tika (through iscc-sdk) extracts from
    // an XLSX holding these stored values and format codes, less the grouping separators,
    // minus signs and `?` of formatted numbers, which the Content-Code drops anyway.

    #[test]
    fn general_uses_excel_digits() {
        for (raw, want) in [
            ("1.00", "1"),
            ("-2.50", "-2.5"),
            ("0.30000000000000004", "0.3"),
            ("12345678.123456789", "12345678.1234568"),
            ("1.23456789012345678", "1.2345678901"),
            ("100000000000000.5", "100000000000001"),
            ("1E15", "1000000000000000"),
            ("1234567890123456", "1.23456789012346E+15"),
            ("123456789012345678", "1.23456789012346E+17"),
            ("1.5E+20", "1.5E+20"),
            ("0.000001", "0.000001"),
            ("0.00048828125", "0.0004882813"),
            ("1.5E-10", "0.0000000001"),
            ("1.1E-15", "0"),
            ("1E-15", "1E-15"),
            ("1E-20", "1E-20"),
        ] {
            assert_eq!(format(raw, "General", false), want, "{raw}");
        }
        assert_eq!(format("0.5", "@", false), "0.5");
    }

    #[test]
    fn placeholders_pad_and_trim() {
        for (raw, code, want) in [
            ("1.2", "0.##", "1.2"),
            ("1", "0.##", "1"),
            ("0.004", "0.##", "0"),
            ("3.14159", "0.##", "3.14"),
            ("42", "00000", "00042"),
            ("4.6", "00000", "00005"),
            ("123456", "00000", "123456"),
            ("1.5", "000.00", "001.50"),
            ("0.5", "#.##", "0.5"),
            ("12.345", "#.##", "12.35"),
            ("0.5", ".00", ".50"),
            ("0.4", "#", "0"),
            ("2.5", "#", "3"),
            ("1234.56789", "#,##0.0##", "1234.568"),
            ("0.1234", "0.0#%", "12.34%"),
            ("0.5", "0.0#%", "50.0%"),
            ("1.25", "0.0?", "1.3"),
            ("1.5", "0.??", "2"),
            ("5", "??0", "5"),
        ] {
            assert_eq!(format(raw, code, false), want, "{raw} as {code}");
        }
    }

    #[test]
    fn rounding_is_half_up_on_excel_digits() {
        assert_eq!(format("1.005", "0.00", false), "1.01");
        assert_eq!(format("1.0049999999999999", "0.00", false), "1.01");
        assert_eq!(format("2.675", "0.00", false), "2.68");
        assert_eq!(format("-0.005", "0.00", false), "0.01");
        assert_eq!(format("0.12345", "0.00%", false), "12.35%");
        assert_eq!(format("0.000123456", "0.00%", false), "0.01%");
        assert_eq!(
            format("1E25", "0.00", false),
            "10000000000000000000000000.00"
        );
    }

    #[test]
    fn scientific_carries_into_the_exponent() {
        assert_eq!(format("999.9", "0.00E+00", false), "1.00E+03");
        assert_eq!(format("1000", "0.00E+00", false), "1.00E+03");
        assert_eq!(format("0.00099999", "0.00E+00", false), "1.00E-03");
        assert_eq!(format("0", "0.00E+00", false), "0.00E+00");
        assert_eq!(format("12345", "0.###E+00", false), "1.235E+04");
        assert_eq!(format("1000", "0.###E+00", false), "1E+03");
    }

    #[test]
    fn times_round_to_the_millisecond_and_cut_to_the_second() {
        for (raw, code, want) in [
            ("46291.1", "hh:mm:ss", "02:24:00"),
            ("0.500007", "hh:mm:ss", "12:00:00"),
            ("0.99999999", "hh:mm:ss", "23:59:59"),
            (
                "46290.99999999999",
                "yyyy-mm-dd hh:mm:ss",
                "2026-09-26 00:00:00",
            ),
            ("46291.1", "h:mm:ss.00", "2:24:00.00"),
            ("0.500011", "h:mm:ss.0", "12:00:00.9"),
            ("0.5000055", "h:mm:ss.0", "12:00:00.5"),
        ] {
            assert_eq!(format(raw, code, false), want, "{raw} as {code}");
        }
    }

    #[test]
    fn elapsed_time_is_cut_down_and_padded() {
        for (raw, code, want) in [
            ("0.7916666666666666", "[h]:mm", "19:00"),
            ("2.9999999", "[h]:mm", "72:59"),
            ("0.53", "[h]:mm", "12:43"),
            ("1.25", "[h]:mm:ss", "30:00:00"),
            ("0.0005", "[ss]", "43"),
            ("0.2", "[h]", "4"),
            ("0.2", "[hh]:mm", "04:48"),
            ("2.5", "[hh]:mm", "60:00"),
            ("0.0005", "[m]", "0"),
            ("0.0005", "[mm]:ss", "00:43"),
        ] {
            assert_eq!(format(raw, code, false), want, "{raw} as {code}");
        }
    }

    #[test]
    fn dates_and_times() {
        // 46291 is 2026-09-26, a Saturday; .75 is 18:00.
        assert_eq!(format("46291", "yyyy\\-mm\\-dd", false), "2026-09-26");
        assert_eq!(format("46291", "m/d/yy", false), "9/26/26");
        assert_eq!(format("46291", "dd.mm.yyyy", false), "26.09.2026");
        assert_eq!(format("46291", "mmm d, yyyy", false), "Sep 26, 2026");
        assert_eq!(
            format("46291", "dddd, mmmm d", false),
            "Saturday, September 26"
        );
        assert_eq!(format("46291.75", "h:mm AM/PM", false), "6:00 PM");
        assert_eq!(format("46291.75", "hh:mm:ss", false), "18:00:00");
        assert_eq!(format("0.3", "h:mm:ss", false), "7:12:00");
        assert_eq!(format("0.500005", "h:mm:ss.00", false), "12:00:00.43");
        assert_eq!(format("1.5", "[h]:mm", false), "36:00");
        assert_eq!(format("1", "yyyy-mm-dd", false), "1900-01-01");
        assert_eq!(format("0", "yyyy-mm-dd", true), "1904-01-01");
    }

    #[test]
    fn builtin_formats_resolve() {
        assert_eq!(builtin(14), Some("m/d/yy"));
        assert_eq!(format("46291", builtin(14).unwrap(), false), "9/26/26");
        assert_eq!(builtin(164), None);
    }
}
