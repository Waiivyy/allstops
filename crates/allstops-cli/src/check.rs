//! Run the independent verifier on an itinerary before anything is shown.

use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::Clustering;
use allstops_gtfs::feed::Feed;
use allstops_gtfs::select::{Selection, select};
use allstops_verify::{Context, Report, parse, verify};
use anyhow::Result;

use crate::plan_input::visit_types;

pub fn check_json(
    feed: &Feed,
    clustering: &Clustering,
    selection: &Selection,
    feed_sha256: Option<&str>,
    json: &str,
) -> Result<Report> {
    let cal = ServiceCalendar::new(feed);
    let targets: Vec<String> = select(feed, clustering, selection)?
        .into_iter()
        .map(|i| clustering.stations[i as usize].id.clone())
        .collect();
    let types = visit_types(feed, selection)?;
    let ctx = Context {
        feed,
        calendar: &cal,
        clustering,
        targets: &targets,
        visit_types: &types,
        feed_sha256,
    };
    let it = parse(json)?;
    Ok(verify(&ctx, &it))
}

pub fn print_report(report: &Report) {
    for v in &report.violations {
        let leg = v
            .leg
            .map(|l| format!("leg {l}"))
            .unwrap_or_else(|| "itinerary".into());
        eprintln!("  {} [{leg}] {}", v.code, v.message);
    }
}
