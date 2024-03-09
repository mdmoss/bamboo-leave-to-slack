use anyhow::Result;
use base64::Engine;
use chrono::{Datelike, Days, NaiveDate, Weekday};
use itertools::Itertools;
use serde::Deserialize;
use std::env;

fn main() {
    let bamboo_company_domain = require_from_env("BAMBOO_COMPANY_DOMAIN");
    let bamboo_api_key = require_from_env("BAMBOO_API_KEY");

    //let today = chrono::Local::now().date_naive().checked_add_days(Days::new(2)).expect("msg");
    let today = chrono::NaiveDate::parse_from_str("2024-04-01", "%Y-%m-%d").expect("");

    let mut leave = fetch_leave_from_bamboo(bamboo_company_domain, bamboo_api_key, today)
        .expect("failed to fetch leave from Bamboo");

    leave.sort_unstable_by_key(|v| (v.r#type.clone(), v.name.clone(), v.start.clone(), v.end.clone()));

    println!("pre-condense");
    for l in &leave {
        println!("{:?}", l)
    }

    let mut leave = first_contiguous_period(&mut leave);

    println!("post-condense");
    for l in &leave {
        println!("{:?}", l)
    }

    send_to_slack(&mut leave, today).expect("failed to send leave to slack");
}

#[derive(Deserialize, Debug, Clone)]
struct LeavePeriod {
    r#type: String, // I've observed "timeOff" and "holiday", but there might be more.
    name: String,
    start: NaiveDate,
    end: NaiveDate,
}

fn fetch_leave_from_bamboo(domain: String, api_key: String, day: NaiveDate) -> Result<Vec<LeavePeriod>> {
    let url = format!(
        "https://api.bamboohr.com/api/gateway.php/{}/v1/time_off/whos_out/",
        domain
    );
    let leave = ureq::get(url.as_str())
        .set("Accept", "application/json")
        .set("Authorization", &basic_auth_header(api_key.as_str(), "x"))
        .query("start", day.to_string().as_str())
        .call()?
        .into_json::<Vec<LeavePeriod>>()?;

    Ok(leave)
}

/// Returns the first contiguous period of leave for each user (grouping by name). // TODO group by id.
///
/// Leave periods are adjacent if they:
/// - Occur on the same day
/// - Occur on adjacent days
/// - Occur with only a weekend in-between.
fn first_contiguous_period(leave: &mut Vec<LeavePeriod>) -> Vec<LeavePeriod> {
    leave.sort_by_key(|l| (l.start.to_string(), l.end.to_string()));

    let a = leave
        .into_iter()
        .into_grouping_map_by(|l| l.name.to_string())
        .fold_first(|a, _, b| {
            // Merge a and b if they are the same, adjacent, or separated by a weekend.
            if same_or_adjacent_workdays(a.end, b.start) {
                // Extend a to cover both a and b.
                a.end = b.end.clone();
            }
            a
        });

    a.into_values().map(|v| v.clone()).collect_vec()
}

fn same_or_adjacent_workdays(a: NaiveDate, b: NaiveDate) -> bool {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };

    a == b || // Same day
    a.succ_opt().is_some_and(|aa| aa == b) || // Next day
    (a.weekday() == Weekday::Fri && a.checked_add_days(Days::new(3)).is_some_and(|aa| aa == b))
    // Crossing a weekend
}

fn send_to_slack(leave: &mut Vec<LeavePeriod>, today: NaiveDate) -> Result<()> {
    leave.sort_by_key(|l| (l.r#type.to_string(), l.name.to_string()));
    let lines: Vec<String> = leave
        .into_iter()
        .filter(|l| l.start <= today)
        .map(|l| {
            format!(
                "* {}{}",
                l.name,
                if l.end != l.start {
                    format!(" _(until {})_", l.end.succ_opt().expect("invalid date").format("%A %-d %B"))
                } else {
                    "".to_string()
                }
            )
        })
        .collect();

    let message = if !lines.is_empty() {
        format!("*Who's off today?*\n{}", lines.join("\n"))
    } else {
        "Nobody is off today! :tada:".to_string()
    };

    println!("{}", message);

    Ok(())
}

fn require_from_env(key: &str) -> String {
    env::var(key).expect(format!("missing required environment variable: {}", key).as_str())
}

fn basic_auth_header(username: &str, password: &str) -> String {
    "Basic ".to_string()
        + base64::prelude::BASE64_STANDARD
            .encode(format!("{}:{}", username, password))
            .as_str()
}
