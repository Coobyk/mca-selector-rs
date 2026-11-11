//! Text/value parsing shared by filters, overlays and the changer
//! (upstream `util.TextHelper` + the name-normalization rules).

/// game ticks per second (InhabitedTime/LastUpdate are tick counts).
pub const TICKS_PER_SECOND: i64 = 20;

/// TextHelper.parseDuration — returns seconds. e.g. "1d", "2h30m".
pub fn parse_duration(d: &str) -> Result<i64, String> {
    const UNITS: &[(&[&str], i64)] = &[
        (&["years", "year", "y"], 31536000),
        (&["months", "month"], 2592000),
        (&["days", "day", "d"], 86400),
        (&["hours", "hour", "h"], 3600),
        (&["minutes", "minute", "mins", "min"], 60),
        (&["seconds", "second", "secs", "sec", "s"], 1),
    ];
    let mut rest = d.trim().to_string();
    let mut total = 0i64;
    let mut matched_any = false;
    while !rest.is_empty() {
        let rest2 = rest.trim_start().to_string();
        rest = rest2;
        if rest.is_empty() {
            break;
        }
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        let num: i64 = digits.parse().map_err(|_| rest.clone())?;
        let after: String = rest.chars().skip(digits.len()).collect();
        let unit_str: String = after.chars().take_while(|c| c.is_alphabetic()).collect();
        if unit_str.is_empty() {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        let mut matched = false;
        for (names, mult) in UNITS {
            if names.iter().any(|n| *n == unit_str) {
                total += num * mult;
                matched = true;
                matched_any = true;
                break;
            }
        }
        if !matched {
            return Err(format!("invalid element in duration string: {rest}"));
        }
        rest = after.chars().skip(unit_str.len()).collect();
    }
    if !matched_any {
        return Err(format!("could not parse anything from duration string: {d}"));
    }
    Ok(total)
}

/// duration string -> ticks (InhabitedTime/LastUpdate values).
pub fn parse_duration_ticks(d: &str) -> Result<i64, String> {
    parse_duration(d).map(|s| s * TICKS_PER_SECOND)
}

/// TextHelper.parseTimestamp — "yyyy-MM-dd[ HH[:mm[:ss]]]" → epoch seconds.
pub fn parse_timestamp(t: &str) -> Result<i32, String> {
    let t = t.trim();
    let (date, time) = match t.split_once(' ') {
        Some((d, tm)) => (d, tm),
        None => (t, "00:00:00"),
    };
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return Err("could not parse date time".into());
    }
    let y: i64 = dp[0].parse().map_err(|_| "bad year".to_string())?;
    let mo: i64 = dp[1].parse().map_err(|_| "bad month".to_string())?;
    let da: i64 = dp[2].parse().map_err(|_| "bad day".to_string())?;
    let tp: Vec<&str> = time.split(':').collect();
    let h: i64 = tp.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mi: i64 = tp.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let se: i64 = tp.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let days = days_from_civil(y, mo, da);
    if days < 0 {
        return Err("could not parse date time".into());
    }
    Ok((days * 86400 + h * 3600 + mi * 60 + se) as i32)
}

/// days since epoch (civil algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// add the default "minecraft:" namespace to a name that has none.
pub fn normalize_name(name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("minecraft:{name}")
    }
}

/// names equal, treating a missing namespace as "minecraft:".
pub fn names_eq(a: &str, b: &str) -> bool {
    a == b || (!a.contains(':') && normalize_name(a) == b) || (!b.contains(':') && a == normalize_name(b))
}

/// TextHelper.splitWithEscaping: split on `sep`, `esc` escapes any char.
pub fn split_escaped(s: &str, sep: char, esc: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == esc {
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// strip one pair of surrounding double quotes.
pub fn unquote(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}
