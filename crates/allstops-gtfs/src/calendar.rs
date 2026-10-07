//! Which services run on which dates, and where a service day sits on the
//! absolute time line.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate, TimeZone};

use crate::error::{Error, Result};
use crate::feed::{Exception, Feed, ServiceIdx};

pub struct ServiceCalendar {
    weekly: Vec<Option<([bool; 7], NaiveDate, NaiveDate)>>,
    added: HashSet<(ServiceIdx, NaiveDate)>,
    removed: HashSet<(ServiceIdx, NaiveDate)>,
    /// Added dates per service, for range checks without scanning days.
    added_by_service: HashMap<ServiceIdx, Vec<NaiveDate>>,
    /// Number of removed dates per service.
    removed_count: HashMap<ServiceIdx, usize>,
}

impl ServiceCalendar {
    pub fn new(feed: &Feed) -> Self {
        let n = feed.service_ids.len();
        let mut weekly = vec![None; n];
        for c in &feed.calendars {
            weekly[c.service as usize] = Some((c.weekdays, c.start, c.end));
        }
        let mut added = HashSet::new();
        let mut removed = HashSet::new();
        let mut added_by_service: HashMap<ServiceIdx, Vec<NaiveDate>> = HashMap::new();
        let mut removed_count: HashMap<ServiceIdx, usize> = HashMap::new();
        for d in &feed.calendar_dates {
            match d.exception {
                Exception::Added => {
                    if added.insert((d.service, d.date)) {
                        added_by_service.entry(d.service).or_default().push(d.date);
                    }
                }
                Exception::Removed => {
                    if removed.insert((d.service, d.date)) {
                        *removed_count.entry(d.service).or_insert(0) += 1;
                    }
                }
            };
        }
        ServiceCalendar {
            weekly,
            added,
            removed,
            added_by_service,
            removed_count,
        }
    }

    pub fn service_count(&self) -> usize {
        self.weekly.len()
    }

    pub fn is_active(&self, service: ServiceIdx, date: NaiveDate) -> bool {
        if self.removed.contains(&(service, date)) {
            return false;
        }
        if self.added.contains(&(service, date)) {
            return true;
        }
        match self.weekly.get(service as usize).copied().flatten() {
            Some((days, start, end)) => {
                date >= start && date <= end && days[date.weekday().num_days_from_monday() as usize]
            }
            None => false,
        }
    }

    /// How many days from either end of a weekly range must be checked to
    /// find its first or last active day. Every 7 consecutive days contain
    /// each weekday once, and each removed date can rule out at most one
    /// candidate, so `7 * (removed + 1)` days always suffice. This keeps the
    /// work proportional to the input, not to the length of the date range.
    fn scan_limit(&self, service: ServiceIdx) -> i64 {
        7 * (self.removed_count.get(&service).copied().unwrap_or(0) as i64 + 1)
    }

    /// First day in `[from, to]` on which the weekly pattern of `service`
    /// runs, ignoring added dates.
    fn first_weekly(
        &self,
        service: ServiceIdx,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Option<NaiveDate> {
        let (days, start, end) = self.weekly.get(service as usize).copied().flatten()?;
        if !days.iter().any(|&d| d) {
            return None;
        }
        let (lo, hi) = (from.max(start), to.min(end));
        let mut d = lo;
        for _ in 0..self.scan_limit(service) {
            if d > hi {
                return None;
            }
            if self.is_active(service, d) {
                return Some(d);
            }
            d = d.succ_opt()?;
        }
        None
    }

    /// Last day in `[from, to]` on which the weekly pattern of `service`
    /// runs, ignoring added dates.
    fn last_weekly(
        &self,
        service: ServiceIdx,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Option<NaiveDate> {
        let (days, start, end) = self.weekly.get(service as usize).copied().flatten()?;
        if !days.iter().any(|&d| d) {
            return None;
        }
        let (lo, hi) = (from.max(start), to.min(end));
        let mut d = hi;
        for _ in 0..self.scan_limit(service) {
            if d < lo {
                return None;
            }
            if self.is_active(service, d) {
                return Some(d);
            }
            d = d.pred_opt()?;
        }
        None
    }

    /// First and last date on which any service runs, or `None` when no
    /// service ever runs.
    pub fn service_range(&self) -> Option<(NaiveDate, NaiveDate)> {
        let mut lo: Option<NaiveDate> = None;
        let mut hi: Option<NaiveDate> = None;
        let mut widen = |d: NaiveDate| {
            lo = Some(lo.map_or(d, |l| l.min(d)));
            hi = Some(hi.map_or(d, |h| h.max(d)));
        };
        for &(s, d) in &self.added {
            if !self.removed.contains(&(s, d)) {
                widen(d);
            }
        }
        for s in 0..self.weekly.len() as ServiceIdx {
            if let Some(d) = self.first_weekly(s, NaiveDate::MIN, NaiveDate::MAX) {
                widen(d);
            }
            if let Some(d) = self.last_weekly(s, NaiveDate::MIN, NaiveDate::MAX) {
                widen(d);
            }
        }
        Some((lo?, hi?))
    }

    /// Whether `service` runs on any date in `[start, end]`.
    pub fn runs_between(&self, service: ServiceIdx, start: NaiveDate, end: NaiveDate) -> bool {
        if let Some(dates) = self.added_by_service.get(&service)
            && dates
                .iter()
                .any(|&d| d >= start && d <= end && !self.removed.contains(&(service, d)))
        {
            return true;
        }
        self.first_weekly(service, start, end).is_some()
    }
}

/// The range a plan date must fall in. Each end comes from the publisher's
/// declared date in feed_info.txt when given, otherwise from the calendars.
pub fn validity(feed: &Feed, cal: &ServiceCalendar) -> Option<(NaiveDate, NaiveDate)> {
    let declared = feed.feed_info.as_ref();
    let derived = cal.service_range();
    let start = declared
        .and_then(|f| f.start_date)
        .or(derived.map(|d| d.0))?;
    let end = declared.and_then(|f| f.end_date).or(derived.map(|d| d.1))?;
    Some((start, end))
}

/// Error unless `date` lies inside the feed's validity range.
pub fn check_plan_date(feed: &Feed, cal: &ServiceCalendar, date: NaiveDate) -> Result<()> {
    let Some((start, end)) = validity(feed, cal) else {
        return Err(Error::File {
            file: "calendar.txt".into(),
            message: "no service runs on any date".into(),
        });
    };
    if date < start || date > end {
        return Err(Error::DateOutOfRange { date, start, end });
    }
    Ok(())
}

/// Unix time (seconds) of "noon minus 12 hours" on `date` in `tz`: the zero
/// point that GTFS times on that service date count from. Daylight-saving
/// changes never skip or repeat noon, so it is unambiguous; `None` only for
/// a date the time zone skipped entirely (for example Pacific/Apia on
/// 2011-12-30).
pub fn service_day_origin<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> Option<i64> {
    let noon = date.and_hms_opt(12, 0, 0)?;
    let local = tz.from_local_datetime(&noon).earliest()?;
    Some(local.timestamp() - 12 * 3600)
}

/// Seconds to add to a time on service date `day` to express it relative to
/// the origin of service date `reference`. `None` when either date does not
/// exist in the time zone.
pub fn day_offset<Tz: TimeZone>(tz: &Tz, reference: NaiveDate, day: NaiveDate) -> Option<i32> {
    let d = service_day_origin(tz, day)? - service_day_origin(tz, reference)?;
    i32::try_from(d).ok()
}

pub fn days(n: i64) -> Duration {
    Duration::days(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::fixture::minimal_with;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn a_skipped_date_has_no_origin() {
        let apia: chrono_tz::Tz = "Pacific/Apia".parse().unwrap();
        assert_eq!(service_day_origin(&apia, date("2011-12-30")), None);
        assert!(service_day_origin(&apia, date("2011-12-31")).is_some());
        assert_eq!(
            day_offset(&apia, date("2011-12-30"), date("2011-12-31")),
            None
        );
    }

    #[test]
    fn huge_never_running_ranges_are_cheap() {
        // Services with no weekday set over 10,000 years, and one running
        // Mondays over the same span: answered without walking the days.
        let mut cal = String::from(
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WD,1,1,1,1,1,0,0,20261001,20261213\nMON,1,0,0,0,0,0,0,00010101,99991231\n",
        );
        for k in 0..200 {
            cal.push_str(&format!("Z{k},0,0,0,0,0,0,0,00010101,99991231\n"));
        }
        let feed =
            Feed::from_zip_bytes(&minimal_with(&[("calendar.txt", &cal)]), &Limits::default())
                .unwrap();
        let t = std::time::Instant::now();
        let c = ServiceCalendar::new(&feed);
        let range = c.service_range().unwrap();
        assert_eq!(range.0, date("0001-01-01"), "the first Monday of year 1");
        assert!(range.1 >= date("9999-12-25"));
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
    }

    #[test]
    fn removed_dates_push_the_first_active_day() {
        let feed = Feed::from_zip_bytes(
            &minimal_with(&[
                (
                    "calendar.txt",
                    "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
                     WD,1,0,0,0,0,0,0,20261005,20261231\n",
                ),
                (
                    "calendar_dates.txt",
                    "service_id,date,exception_type\nWD,20261005,2\nWD,20261012,2\nWD,20261019,2\n",
                ),
            ]),
            &Limits::default(),
        )
        .unwrap();
        let c = ServiceCalendar::new(&feed);
        assert_eq!(c.service_range().unwrap().0, date("2026-10-26"));
        assert!(c.runs_between(0, date("2026-10-01"), date("2026-10-26")));
        assert!(!c.runs_between(0, date("2026-10-01"), date("2026-10-25")));
    }

    #[test]
    fn one_declared_feed_info_date_is_used_on_its_own() {
        let one = |cols: &str, val: &str| {
            Feed::from_zip_bytes(
                &minimal_with(&[(
                    "feed_info.txt",
                    &format!("feed_publisher_name,feed_publisher_url,feed_lang,{cols}\nP,https://example.org,de,{val}\n"),
                )]),
                &Limits::default(),
            )
            .unwrap()
        };
        let f = one("feed_end_date", "20261015");
        let c = ServiceCalendar::new(&f);
        assert_eq!(
            validity(&f, &c),
            Some((date("2026-10-01"), date("2026-10-15")))
        );
        assert!(check_plan_date(&f, &c, date("2026-11-02")).is_err());
        let f = one("feed_start_date", "20261101");
        let c = ServiceCalendar::new(&f);
        assert_eq!(
            validity(&f, &c),
            Some((date("2026-11-01"), date("2026-12-11")))
        );
        assert!(check_plan_date(&f, &c, date("2026-10-26")).is_err());
    }
}
