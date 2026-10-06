//! Which services run on which dates, and where a service day sits on the
//! absolute time line.

use std::collections::HashSet;

use chrono::{Datelike, Duration, NaiveDate, TimeZone};

use crate::error::{Error, Result};
use crate::feed::{Exception, Feed, ServiceIdx};

pub struct ServiceCalendar {
    weekly: Vec<Option<([bool; 7], NaiveDate, NaiveDate)>>,
    added: HashSet<(ServiceIdx, NaiveDate)>,
    removed: HashSet<(ServiceIdx, NaiveDate)>,
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
        for d in &feed.calendar_dates {
            match d.exception {
                Exception::Added => added.insert((d.service, d.date)),
                Exception::Removed => removed.insert((d.service, d.date)),
            };
        }
        ServiceCalendar {
            weekly,
            added,
            removed,
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
        for (s, w) in self.weekly.iter().enumerate() {
            let Some((_, start, end)) = *w else { continue };
            let s = s as ServiceIdx;
            if let Some(d) = first_active(start, end, |d| self.is_active(s, d)) {
                widen(d);
            }
            if let Some(d) = last_active(start, end, |d| self.is_active(s, d)) {
                widen(d);
            }
        }
        Some((lo?, hi?))
    }

    /// Number of dates in `[start, end]` on which `service` runs.
    pub fn active_days(&self, service: ServiceIdx, start: NaiveDate, end: NaiveDate) -> usize {
        start
            .iter_days()
            .take_while(|d| *d <= end)
            .filter(|d| self.is_active(service, *d))
            .count()
    }
}

fn first_active(
    start: NaiveDate,
    end: NaiveDate,
    f: impl Fn(NaiveDate) -> bool,
) -> Option<NaiveDate> {
    start.iter_days().take_while(|d| *d <= end).find(|d| f(*d))
}

fn last_active(
    start: NaiveDate,
    end: NaiveDate,
    f: impl Fn(NaiveDate) -> bool,
) -> Option<NaiveDate> {
    let mut d = end;
    while d >= start {
        if f(d) {
            return Some(d);
        }
        d = d.pred_opt()?;
    }
    None
}

/// The range a plan date must fall in. The publisher's declared range in
/// feed_info.txt wins when both ends are given; otherwise the range is
/// derived from the calendars.
pub fn validity(feed: &Feed, cal: &ServiceCalendar) -> Option<(NaiveDate, NaiveDate)> {
    if let Some(fi) = &feed.feed_info
        && let (Some(s), Some(e)) = (fi.start_date, fi.end_date)
    {
        return Some((s, e));
    }
    cal.service_range()
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
/// point that GTFS times on that service date count from. Noon is never
/// skipped or repeated by daylight-saving changes, so it is unambiguous.
pub fn service_day_origin<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> i64 {
    let noon = date.and_hms_opt(12, 0, 0).expect("valid time");
    let local = tz
        .from_local_datetime(&noon)
        .earliest()
        .expect("noon exists on every date in real time zones");
    local.timestamp() - 12 * 3600
}

/// Seconds to add to a time on service date `day` to express it relative to
/// the origin of service date `reference`.
pub fn day_offset<Tz: TimeZone>(tz: &Tz, reference: NaiveDate, day: NaiveDate) -> i32 {
    (service_day_origin(tz, day) - service_day_origin(tz, reference)) as i32
}

pub fn days(n: i64) -> Duration {
    Duration::days(n)
}
