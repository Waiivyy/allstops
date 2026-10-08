//! Run the independent verifier on an itinerary before anything is shown.

use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_verify::{Context, Report, RulesIn, parse, verify};
use anyhow::Result;

use crate::plan_input::Basis;

/// The verifier's view of a rules value: the same fields, read through its
/// own types, so the verifier never depends on the solver's.
pub fn rules_for_verifier(rules: &Rules) -> Result<RulesIn> {
    Ok(serde_json::from_value(serde_json::to_value(rules)?)?)
}

/// Verify an itinerary against the feed, stations, targets and walks of
/// `basis`, under `expected_rules` when given.
pub fn check_json(basis: &Basis, expected_rules: Option<&RulesIn>, json: &str) -> Result<Report> {
    let cal = ServiceCalendar::new(&basis.feed);
    let targets = basis.target_ids();
    let ctx = Context {
        feed: &basis.feed,
        calendar: &cal,
        clustering: &basis.clustering,
        targets: &targets,
        visit_types: &basis.visit_types,
        feed_sha256: Some(&basis.feed_ref.sha256),
        expected_rules,
        walks: &basis.walks,
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
