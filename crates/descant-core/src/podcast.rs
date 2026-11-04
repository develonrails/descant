//! Podcast episodes: where the listener is in one, and how a list says so.
//!
//! Spotify reports a played state per episode (`NOT_STARTED`, `IN_PROGRESS`,
//! `COMPLETED`) with a position. The rules here turn that into what a row
//! shows and where playing starts. Dates arrive as civil dates (year, month,
//! day) in the listener's own time zone, so "Today" means their today.

/// Where the listener is in an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    NotStarted,
    /// Started and not finished.
    InProgress {
        position_ms: u32,
        left_ms: u32,
    },
    Played,
}

/// The last minute is outro and credits: stopping there is having heard it.
const TAIL_MS: u32 = 60_000;
/// Going on from where the listener stopped repeats the last few seconds, so
/// the sentence it broke off in is heard again.
const REWIND_MS: u32 = 5_000;
/// How long a new episode counts as new.
const NEW_FOR_DAYS: i64 = 7;

/// Spotify's played state and position, read against the episode's length.
pub fn progress(state: &str, position_ms: u32, duration_ms: u32) -> Progress {
    match state {
        "COMPLETED" => Progress::Played,
        "IN_PROGRESS" if position_ms > 0 => {
            let left_ms = duration_ms.saturating_sub(position_ms);
            if left_ms <= TAIL_MS {
                Progress::Played
            } else {
                Progress::InProgress {
                    position_ms,
                    left_ms,
                }
            }
        }
        _ => Progress::NotStarted,
    }
}

/// Where playing an episode starts.
pub fn start_at(progress: Progress) -> u32 {
    match progress {
        Progress::InProgress { position_ms, .. } => position_ms.saturating_sub(REWIND_MS),
        Progress::NotStarted | Progress::Played => 0,
    }
}

/// How far through, from 0.0 to 1.0, for a progress bar.
pub fn fraction(progress: Progress, duration_ms: u32) -> f64 {
    match progress {
        Progress::NotStarted => 0.0,
        Progress::Played => 1.0,
        Progress::InProgress { .. } if duration_ms == 0 => 0.0,
        Progress::InProgress { position_ms, .. } => f64::from(position_ms) / f64::from(duration_ms),
    }
}

/// "23 min left", never less than a minute.
pub fn left_label(left_ms: u32) -> String {
    format!("{} min left", minutes(left_ms))
}

/// Whole minutes, rounded, never less than one.
fn minutes(ms: u32) -> u32 {
    (ms.saturating_add(30_000) / 60_000).max(1)
}

/// "35 min", or "1 hr 17 min" past the hour.
pub fn duration_label(duration_ms: u32) -> String {
    let m = minutes(duration_ms);
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

/// "3,027 episodes", or "1 episode".
pub fn episode_count_label(n: u32) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped} episode{}", if n == 1 { "" } else { "s" })
}

/// A civil date: year, month (1–12), day (1–31).
pub type Date = (i32, u32, u32);

/// "Today", "Yesterday", "Nov 3", or "Nov 3, 2025" in another year.
pub fn released_label(released: Date, today: Date) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match days(today) - days(released) {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let (year, month, day) = released;
            let month = MONTHS[(month.clamp(1, 12) - 1) as usize];
            if year == today.0 {
                format!("{month} {day}")
            } else {
                format!("{month} {day}, {year}")
            }
        }
    }
}

/// Whether an episode counts as new: not started, and out at most a week.
pub fn is_new(progress: Progress, released: Date, today: Date) -> bool {
    progress == Progress::NotStarted && (0..=NEW_FOR_DAYS).contains(&(days(today) - days(released)))
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm), so
/// dates subtract across months and years.
fn days((year, month, day): Date) -> i64 {
    let (m, d) = (i64::from(month), i64::from(day));
    let y = i64::from(year) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * if m > 2 { m - 3 } else { m + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u32 = 3_600_000;

    #[test]
    fn spotify_s_states_become_progress() {
        assert_eq!(progress("NOT_STARTED", 0, HOUR), Progress::NotStarted);
        assert_eq!(progress("COMPLETED", 0, HOUR), Progress::Played);
        assert_eq!(
            progress("IN_PROGRESS", 600_000, HOUR),
            Progress::InProgress {
                position_ms: 600_000,
                left_ms: HOUR - 600_000
            }
        );
    }

    #[test]
    fn stopping_in_the_last_minute_is_having_heard_it() {
        assert_eq!(
            progress("IN_PROGRESS", HOUR - 59_000, HOUR),
            Progress::Played
        );
        assert!(matches!(
            progress("IN_PROGRESS", HOUR - 61_000, HOUR),
            Progress::InProgress { .. }
        ));
    }

    #[test]
    fn an_unknown_state_or_a_zero_position_is_not_started() {
        assert_eq!(progress("SOMETHING_NEW", 0, HOUR), Progress::NotStarted);
        assert_eq!(progress("IN_PROGRESS", 0, HOUR), Progress::NotStarted);
    }

    #[test]
    fn playing_goes_on_a_little_before_where_it_stopped() {
        let p = progress("IN_PROGRESS", 600_000, HOUR);
        assert_eq!(start_at(p), 595_000);
        assert_eq!(start_at(progress("IN_PROGRESS", 3_000, HOUR)), 0);
        assert_eq!(
            start_at(Progress::Played),
            0,
            "a played episode starts over"
        );
        assert_eq!(start_at(Progress::NotStarted), 0);
    }

    #[test]
    fn the_bar_fills_with_the_part_heard() {
        assert_eq!(fraction(progress("IN_PROGRESS", 900_000, HOUR), HOUR), 0.25);
        assert_eq!(fraction(Progress::Played, HOUR), 1.0);
        assert_eq!(fraction(Progress::NotStarted, HOUR), 0.0);
        assert_eq!(fraction(Progress::Played, 0), 1.0, "no length, no division");
    }

    #[test]
    fn what_is_left_is_counted_in_whole_minutes() {
        assert_eq!(left_label(23 * 60_000 + 20_000), "23 min left");
        assert_eq!(left_label(23 * 60_000 + 40_000), "24 min left");
        assert_eq!(left_label(10_000), "1 min left");
    }

    #[test]
    fn lengths_read_like_spotify_s() {
        assert_eq!(duration_label(2_079_973), "35 min");
        assert_eq!(duration_label(4_612_023), "1 hr 17 min");
        assert_eq!(duration_label(HOUR), "1 hr");
        assert_eq!(duration_label(20_000), "1 min");
    }

    #[test]
    fn release_dates_are_relative_when_near() {
        let today = (2026, 11, 4);
        assert_eq!(released_label((2026, 11, 4), today), "Today");
        assert_eq!(released_label((2026, 11, 3), today), "Yesterday");
        assert_eq!(released_label((2026, 11, 2), today), "Nov 2");
        assert_eq!(released_label((2025, 12, 30), today), "Dec 30, 2025");
        assert_eq!(released_label((2026, 3, 1), (2026, 3, 2)), "Yesterday");
        assert_eq!(
            released_label((2025, 12, 31), (2026, 1, 1)),
            "Yesterday",
            "across a new year"
        );
    }

    #[test]
    fn episode_counts_read_with_thousands() {
        assert_eq!(episode_count_label(3027), "3,027 episodes");
        assert_eq!(episode_count_label(1), "1 episode");
        assert_eq!(episode_count_label(1_234_567), "1,234,567 episodes");
        assert_eq!(episode_count_label(999), "999 episodes");
    }

    #[test]
    fn new_means_unheard_and_from_this_week() {
        let today = (2026, 11, 4);
        assert!(is_new(Progress::NotStarted, (2026, 11, 4), today));
        assert!(is_new(Progress::NotStarted, (2026, 10, 28), today));
        assert!(!is_new(Progress::NotStarted, (2026, 10, 27), today));
        assert!(!is_new(Progress::Played, (2026, 11, 4), today));
        assert!(!is_new(
            progress("IN_PROGRESS", 600_000, HOUR),
            (2026, 11, 4),
            today
        ));
    }
}
