//! Dates in text, for the temporal channel: rules only, no model, so the same text always gives
//! the same dates and nothing is learned from any benchmark.
//!
//! `spans` finds calendar dates and periods in a text and resolves relative ones ("yesterday",
//! "three weeks ago", "last May") against an anchor: the time the memory was said (its session
//! date) or the time a question is asked. Each span is a half-open day range [from, to).
//! `intent` says whether a question asks about time (when, how long, before, after, first, last).

use chrono::{Datelike, Duration, Months, NaiveDate, Weekday};

use crate::store::parse_time;
use crate::types::Memory;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: NaiveDate,
    /// Exclusive.
    pub to: NaiveDate,
    /// The words the span came from.
    pub text: String,
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Mon),
    ("tuesday", Weekday::Tue),
    ("wednesday", Weekday::Wed),
    ("thursday", Weekday::Thu),
    ("friday", Weekday::Fri),
    ("saturday", Weekday::Sat),
    ("sunday", Weekday::Sun),
];
const NUMBERS: [&str; 13] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve",
];

fn month(word: &str) -> Option<u32> {
    // A full name, its three-letter abbreviation, or "sept". Whether "may" or "march" is a month
    // is decided by the words around it (see `spans`).
    let w = word.trim_end_matches('.');
    MONTHS
        .iter()
        .position(|m| {
            *m == w || (w.len() == 3 && m.starts_with(w)) || (w == "sept" && *m == "september")
        })
        .map(|i| i as u32 + 1)
}

fn number(word: &str) -> Option<i64> {
    match word {
        "a" | "an" => Some(1),
        "couple" => Some(2),
        "few" => Some(3),
        _ => word
            .parse()
            .ok()
            .or_else(|| NUMBERS.iter().position(|n| *n == word).map(|i| i as i64)),
    }
    // Bounded, so date arithmetic never leaves chrono's range ("100000000 days ago", "-5 months ago").
    .filter(|n| (0..=1000).contains(n))
}

/// "may" and "march" are also a verb: as a month they need an ordinal day ("3rd"), "of", or a year.
fn ambiguous(month_word: &str) -> bool {
    matches!(month_word, "may" | "march")
}

fn ordinal(word: &str) -> bool {
    let w = word.trim_end_matches([',', '.']);
    ["st", "nd", "rd", "th"].iter().any(|s| w.ends_with(s))
        && w.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn day_of(word: &str) -> Option<u32> {
    let d: u32 = word
        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == ',')
        .parse()
        .ok()?;
    (1..=31).contains(&d).then_some(d)
}

fn year_of(word: &str) -> Option<i32> {
    let y: i32 = word.trim_end_matches([',', '.', ')', ';']).parse().ok()?;
    (1900..=2100).contains(&y).then_some(y)
}

fn day(d: NaiveDate, text: String) -> Span {
    Span {
        from: d,
        to: d + Duration::days(1),
        text,
    }
}

fn month_span(y: i32, m: u32, text: String) -> Option<Span> {
    let from = NaiveDate::from_ymd_opt(y, m, 1)?;
    Some(Span {
        to: from + Months::new(1),
        from,
        text,
    })
}

fn week_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

/// Words of `text`, lowercased, with their punctuation kept for the date rules to trim.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == '(' || c == '[')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Every date or period in `text`, relative ones resolved against `anchor`.
pub fn spans(text: &str, anchor: NaiveDate) -> Vec<Span> {
    let w = words(text);
    let mut out = vec![];
    let mut i = 0;
    while i < w.len() {
        let clean = |j: usize| {
            w.get(j).map(|s| {
                s.trim_end_matches([',', '.', '?', '!', ';', ':', ')'])
                    .to_owned()
            })
        };
        let cur = clean(i).unwrap_or_default();
        let next = clean(i + 1);
        let span_text = |n: usize| w[i..(i + n).min(w.len())].join(" ");
        // ISO and slashed dates: 2023-05-20, 2023/05/20.
        let parts: Vec<&str> = cur.split(['-', '/']).collect();
        if parts.len() == 3 && parts[0].len() == 4 {
            if let (Ok(y), Ok(m), Ok(d)) = (parts[0].parse(), parts[1].parse(), parts[2].parse()) {
                if let Some(date) = NaiveDate::from_ymd_opt(y, m, d) {
                    out.push(day(date, cur.clone()));
                    i += 1;
                    continue;
                }
            }
        }
        // 20 July 2023, 20 July, 2023, 20th of July.
        if let Some(d) = day_of(&cur) {
            let (mi, of) = if next.as_deref() == Some("of") {
                (i + 2, 1)
            } else {
                (i + 1, 0)
            };
            if let Some(m) = clean(mi).as_deref().and_then(month) {
                let year = clean(mi + 1).as_deref().and_then(year_of);
                let (y, n) = match year {
                    Some(y) => (y, 3 + of),
                    None => (anchor.year(), 2 + of),
                };
                let clear = !clean(mi).as_deref().is_some_and(ambiguous)
                    || of == 1
                    || ordinal(&cur)
                    || year.is_some();
                if let (true, Some(date)) = (clear, NaiveDate::from_ymd_opt(y, m, d)) {
                    out.push(day(date, span_text(n)));
                    i += n;
                    continue;
                }
            }
        }
        // July 20, 2023; July 20th; July 2023; in July. A capitalised "May" in the source is a
        // month only with a day or a year next to it, since "may" is also a verb.
        if let Some(m) = month(&cur) {
            if let Some(d) = next.as_deref().and_then(day_of) {
                let year = clean(i + 2).as_deref().and_then(year_of);
                let (y, n) = match year {
                    Some(y) => (y, 3),
                    None => (anchor.year(), 2),
                };
                let clear =
                    !ambiguous(&cur) || year.is_some() || next.as_deref().is_some_and(ordinal);
                if let (true, Some(date)) = (clear, NaiveDate::from_ymd_opt(y, m, d)) {
                    out.push(day(date, span_text(n)));
                    i += n;
                    continue;
                }
            }
            if let Some(y) = next.as_deref().and_then(year_of) {
                if let Some(s) = month_span(y, m, span_text(2)) {
                    out.push(s);
                    i += 2;
                    continue;
                }
            }
            let prev = if i > 0 { clean(i - 1) } else { None };
            if cur != "may"
                && matches!(
                    prev.as_deref(),
                    Some(
                        "in" | "since"
                            | "until"
                            | "during"
                            | "last"
                            | "next"
                            | "this"
                            | "early"
                            | "late"
                            | "mid"
                    )
                )
            {
                // "last May" is the latest May before the anchor's month; "next May" the first after.
                let y = match prev.as_deref() {
                    Some("last") if m >= anchor.month() => anchor.year() - 1,
                    Some("next") if m <= anchor.month() => anchor.year() + 1,
                    _ => anchor.year(),
                };
                if let Some(s) = month_span(y, m, w[i - 1..=i].join(" ")) {
                    out.push(s);
                    i += 1;
                    continue;
                }
            }
        }
        // A bare year after a preposition: in 2022, since 2019.
        if let Some(y) = year_of(&cur) {
            let prev = if i > 0 { clean(i - 1) } else { None };
            if matches!(
                prev.as_deref(),
                Some("in" | "since" | "until" | "during" | "from")
            ) {
                let from = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
                out.push(Span {
                    from,
                    to: NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap(),
                    text: cur.clone(),
                });
                i += 1;
                continue;
            }
        }
        match cur.as_str() {
            "today" | "tonight" => out.push(day(anchor, cur.clone())),
            "yesterday" => out.push(day(anchor - Duration::days(1), cur.clone())),
            "tomorrow" => out.push(day(anchor + Duration::days(1), cur.clone())),
            "last" | "next" | "this" => {
                let sign = match cur.as_str() {
                    "last" => -1,
                    "next" => 1,
                    _ => 0,
                };
                let unit = next.clone().unwrap_or_default();
                let found = match unit.as_str() {
                    "week" => Some(Span {
                        from: week_of(anchor) + Duration::weeks(sign),
                        to: week_of(anchor) + Duration::weeks(sign + 1),
                        text: span_text(2),
                    }),
                    "weekend" => {
                        let sat = week_of(anchor) + Duration::days(5) + Duration::weeks(sign);
                        Some(Span {
                            from: sat,
                            to: sat + Duration::days(2),
                            text: span_text(2),
                        })
                    }
                    "month" => {
                        let first =
                            NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1).unwrap();
                        let from = if sign < 0 {
                            first - Months::new(1)
                        } else {
                            first + Months::new(sign as u32)
                        };
                        Some(Span {
                            from,
                            to: from + Months::new(1),
                            text: span_text(2),
                        })
                    }
                    "year" => {
                        let y = anchor.year() + sign as i32;
                        Some(Span {
                            from: NaiveDate::from_ymd_opt(y, 1, 1).unwrap(),
                            to: NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap(),
                            text: span_text(2),
                        })
                    }
                    u => WEEKDAYS.iter().find(|(n, _)| *n == u).map(|(_, wd)| {
                        // "last Friday": the latest Friday before the anchor; "next Friday": the first after.
                        let back = (anchor.weekday().num_days_from_monday() as i64
                            - wd.num_days_from_monday() as i64)
                            .rem_euclid(7);
                        let d = match sign {
                            -1 => anchor - Duration::days(if back == 0 { 7 } else { back }),
                            1 => anchor + Duration::days(if back == 0 { 7 } else { 7 - back }),
                            _ => week_of(anchor) + Duration::days(wd.num_days_from_monday() as i64),
                        };
                        day(d, span_text(2))
                    }),
                };
                if let Some(s) = found {
                    out.push(s);
                    i += 2;
                    continue;
                }
            }
            _ => {
                // N days/weeks/months/years ago: the day, week, month or year that far back.
                if let Some(n) = number(&cur) {
                    // "a couple of weeks ago": skip the "of".
                    let skip = usize::from(next.as_deref() == Some("of"));
                    let (unit, ago) =
                        (clean(i + 1 + skip).unwrap_or_default(), clean(i + 2 + skip));
                    if ago.as_deref() == Some("ago") {
                        let unit = unit.trim_end_matches('s');
                        let found = match unit {
                            "day" => Some(day(anchor - Duration::days(n), span_text(3 + skip))),
                            "week" => {
                                let from = week_of(anchor - Duration::weeks(n));
                                Some(Span {
                                    from,
                                    to: from + Duration::weeks(1),
                                    text: span_text(3 + skip),
                                })
                            }
                            "month" => {
                                let first =
                                    NaiveDate::from_ymd_opt(anchor.year(), anchor.month(), 1)
                                        .unwrap()
                                        - Months::new(n as u32);
                                Some(Span {
                                    from: first,
                                    to: first + Months::new(1),
                                    text: span_text(3 + skip),
                                })
                            }
                            "year" => {
                                let y = anchor.year() - n as i32;
                                Some(Span {
                                    from: NaiveDate::from_ymd_opt(y, 1, 1).unwrap(),
                                    to: NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap(),
                                    text: span_text(3 + skip),
                                })
                            }
                            _ => None,
                        };
                        if let Some(s) = found {
                            out.push(s);
                            i += 3 + skip;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// What a question asks about time, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// "When did ...", "what date", "what year": the answer is a date.
    When,
    /// "How long", "how many days/weeks/months/years": the answer is a duration.
    Duration,
    /// "first", "last", "before", "after", "earlier", "most recent": the answer depends on order.
    Order,
}

pub fn intent(question: &str) -> Option<Intent> {
    let q = question.to_lowercase();
    let has = |p: &str| {
        q.split(|c: char| !c.is_alphanumeric())
            .collect::<Vec<_>>()
            .windows(p.split(' ').count())
            .any(|w| w.join(" ") == p)
    };
    if q.trim_start().starts_with("when")
        || [
            "what date",
            "what year",
            "what month",
            "which year",
            "which month",
            "what day",
        ]
        .iter()
        .any(|p| has(p))
    {
        Some(Intent::When)
    } else if [
        "how long",
        "how many days",
        "how many weeks",
        "how many months",
        "how many years",
    ]
    .iter()
    .any(|p| has(p))
    {
        Some(Intent::Duration)
    } else if [
        "first",
        "last",
        "before",
        "after",
        "earlier",
        "later",
        "most recent",
        "latest",
        "earliest",
    ]
    .iter()
    .any(|p| has(p))
    {
        Some(Intent::Order)
    } else {
        None
    }
}

/// A date usable as an anchor: years 1 to 9999, so date arithmetic stays inside chrono's range.
pub fn plain(d: NaiveDate) -> Option<NaiveDate> {
    (1..=9999).contains(&d.year()).then_some(d)
}

/// When a memory was said: its source's time (`metadata.timestamp`, any format `spans` reads),
/// else the start of its valid time, else when Hyperag recorded it.
pub fn anchor_of(m: &Memory) -> Option<NaiveDate> {
    let recorded = parse_time(&m.recorded_at)
        .map(|t| t.date_naive())
        .and_then(plain);
    // An ISO timestamp first ("2023-05-20T10:00:00Z"), then any date `spans` reads ("8 May, 2023").
    let said = m
        .metadata
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(|t| {
            parse_time(t)
                .map(|t| t.date_naive())
                .and_then(plain)
                .or_else(|| spans(t, recorded?).first().map(|s| s.from))
        });
    said.or_else(|| {
        m.valid_from
            .as_deref()
            .and_then(parse_time)
            .map(|t| t.date_naive())
            .and_then(plain)
    })
    .or(recorded)
}

/// The times a memory talks about, other than the day it was said: a source that writes its own
/// date into the text ("8 May, 2023: Priya said") does not make every line look dated.
pub fn mentioned(m: &Memory) -> (Option<NaiveDate>, Vec<Span>) {
    let Some(anchor) = anchor_of(m) else {
        return (None, vec![]);
    };
    let found = spans(&m.text, anchor)
        .into_iter()
        .filter(|s| !(s.from == anchor && s.to == anchor + Duration::days(1)))
        .collect();
    (Some(anchor), found)
}

/// "said 2023-05-08; yesterday = 2023-05-07": what a reader needs to place a memory in time.
pub fn describe(m: &Memory) -> Option<String> {
    let (said, found) = mentioned(m);
    let dates: Vec<String> = found
        .iter()
        .map(|s| {
            if s.to == s.from + Duration::days(1) {
                format!("{} = {}", s.text, s.from)
            } else {
                format!("{} = {} to {}", s.text, s.from, s.to - Duration::days(1))
            }
        })
        .collect();
    Some(format!(
        "said {}{}",
        said?,
        if dates.is_empty() {
            String::new()
        } else {
            format!("; {}", dates.join("; "))
        }
    ))
}

pub fn overlaps(a: &Span, b: &Span) -> bool {
    a.from < b.to && b.from < a.to
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn ranges(text: &str, anchor: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
        spans(text, anchor)
            .into_iter()
            .map(|s| (s.from, s.to))
            .collect()
    }

    #[test]
    fn absolute_dates() {
        let a = d(2023, 8, 1);
        assert_eq!(
            ranges("9:10 am on 8 May, 2023: Priya said", a),
            [(d(2023, 5, 8), d(2023, 5, 9))]
        );
        assert_eq!(
            ranges("[2023/05/20 (Sat) 02:21] user: hi", a),
            [(d(2023, 5, 20), d(2023, 5, 21))]
        );
        assert_eq!(
            ranges("on July 20th, 2022 we moved", a),
            [(d(2022, 7, 20), d(2022, 7, 21))]
        );
        assert_eq!(
            ranges("the 3rd of June", a),
            [(d(2023, 6, 3), d(2023, 6, 4))]
        );
        assert_eq!(
            ranges("back in March 2021", a),
            [(d(2021, 3, 1), d(2021, 4, 1))]
        );
        assert_eq!(
            ranges("we met in 2019.", a),
            [(d(2019, 1, 1), d(2020, 1, 1))]
        );
        assert_eq!(ranges("2023-02-29 is not a date", a), []);
    }

    #[test]
    fn relative_dates_resolve_against_the_anchor() {
        let a = d(2023, 5, 10); // a Wednesday
        assert_eq!(
            ranges("I went yesterday", a),
            [(d(2023, 5, 9), d(2023, 5, 10))]
        );
        assert_eq!(ranges("two days ago", a), [(d(2023, 5, 8), d(2023, 5, 9))]);
        assert_eq!(ranges("a week ago", a), [(d(2023, 5, 1), d(2023, 5, 8))]);
        assert_eq!(ranges("last week", a), [(d(2023, 5, 1), d(2023, 5, 8))]);
        assert_eq!(ranges("last weekend", a), [(d(2023, 5, 6), d(2023, 5, 8))]);
        assert_eq!(ranges("last Friday", a), [(d(2023, 5, 5), d(2023, 5, 6))]);
        assert_eq!(ranges("next Monday", a), [(d(2023, 5, 15), d(2023, 5, 16))]);
        assert_eq!(ranges("last month", a), [(d(2023, 4, 1), d(2023, 5, 1))]);
        assert_eq!(
            ranges("three months ago", a),
            [(d(2023, 2, 1), d(2023, 3, 1))]
        );
        assert_eq!(ranges("last year", a), [(d(2022, 1, 1), d(2023, 1, 1))]);
        assert_eq!(ranges("last June", a), [(d(2022, 6, 1), d(2022, 7, 1))]);
        assert_eq!(ranges("in April", a), [(d(2023, 4, 1), d(2023, 5, 1))]);
        assert_eq!(
            ranges("a couple of weeks ago", a),
            [(d(2023, 4, 24), d(2023, 5, 1))]
        );
        assert_eq!(
            ranges("on the 2nd of May and on May 3rd", a),
            [
                (d(2023, 5, 2), d(2023, 5, 3)),
                (d(2023, 5, 3), d(2023, 5, 4))
            ]
        );
        assert_eq!(
            ranges("since 2 May 2023", a),
            [(d(2023, 5, 2), d(2023, 5, 3))]
        );
    }

    #[test]
    fn words_that_are_not_dates() {
        let a = d(2023, 5, 10);
        assert_eq!(ranges("You may go. I may visit in may.", a), []);
        assert_eq!(ranges("I have 3 cats and 2023 photos", a), []);
        assert_eq!(ranges("the last time we spoke", a), []);
        assert_eq!(ranges("these 2 may help, and the 3 may come later", a), []);
        assert_eq!(ranges("we march 5 miles", a), []);
        assert_eq!(ranges("a crowd of 2000 people grew by 2000", a), []);
        // Out-of-range counts give nothing instead of failing.
        assert_eq!(
            ranges("100000000 days ago, 300000 years ago, -5 months ago", a),
            []
        );
    }

    #[test]
    fn mentioned_times_skip_the_day_a_memory_was_said() {
        let m = |text: &str| Memory {
            id: "x".into(),
            scope: "s".into(),
            kind: "turn".into(),
            version: 1,
            text: text.into(),
            source: "Priya".into(),
            author: "bench".into(),
            valid_from: None,
            valid_to: None,
            recorded_at: "2026-10-24T00:00:00.000Z".into(),
            status: "active".into(),
            status_reason: None,
            superseded_by: None,
            metadata: serde_json::from_str(r#"{"timestamp": "9:10 am on 8 May, 2023"}"#).unwrap(),
        };
        let (said, found) = mentioned(&m(
            "9:10 am on 8 May, 2023: Priya said, \"I renewed my passport yesterday.\"",
        ));
        assert_eq!(said, Some(d(2023, 5, 8)));
        assert_eq!(
            found.iter().map(|s| s.from).collect::<Vec<_>>(),
            [d(2023, 5, 7)]
        );
        // Only the source's own date prefix: nothing is mentioned.
        assert!(
            mentioned(&m("9:10 am on 8 May, 2023: Priya said, \"Hi!\""))
                .1
                .is_empty()
        );
        // An ISO timestamp with a time anchors too.
        let mut iso = m("we shipped it yesterday");
        iso.metadata
            .insert("timestamp".into(), "2023-05-20T10:00:00Z".into());
        assert_eq!(anchor_of(&iso), Some(d(2023, 5, 20)));
        // A timestamp at chrono's edge is not an anchor; the next source is used, and nothing panics.
        let mut edge = m("see you tomorrow");
        edge.metadata
            .insert("timestamp".into(), "+262142-12-31T00:00:00".into());
        assert_eq!(anchor_of(&edge), Some(d(2026, 10, 24)));
        assert_eq!(mentioned(&edge).1[0].from, d(2026, 10, 25));
        // No timestamp: the recorded time anchors.
        let mut plain = m("we shipped it yesterday");
        plain.metadata.clear();
        assert_eq!(mentioned(&plain).1[0].from, d(2026, 10, 23));
    }

    #[test]
    fn question_intent() {
        assert_eq!(
            intent("When did Priya renew her passport?"),
            Some(Intent::When)
        );
        assert_eq!(intent("In what year did she move?"), Some(Intent::When));
        assert_eq!(
            intent("How long has Tomas been learning the cello?"),
            Some(Intent::Duration)
        );
        assert_eq!(
            intent("Which did I buy first, the bike or the car?"),
            Some(Intent::Order)
        );
        assert_eq!(intent("What is Priya's favourite tea?"), None);
    }
}
