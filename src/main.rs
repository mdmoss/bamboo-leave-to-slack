use anyhow::Result;
use base64::Engine;
use chrono::{Datelike, Days, NaiveDate, Weekday};
use clap::Parser;
use itertools::Itertools;
use serde::Deserialize;
use std::env;

// If you take more than a year of leave, we might miss it. Sorry.
const LEAVE_LOOKAHEAD: Days = Days::new(365);

fn main() {
    let args: Args = Args::parse();

    let bamboo_company_domain = require_from_env("BAMBOO_COMPANY_DOMAIN");
    let bamboo_api_key = require_from_env("BAMBOO_API_KEY");
    let slack_webhook_url = require_from_env("SLACK_WEBHOOK_URL");

    let date = match args.date {
        Some(date) => {
            chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").expect("Invalid date argument (expected YYYY-MM-DD)")
        }
        None => chrono::Local::now().date_naive(),
    };

    println!("sending leave for {}", date);

    let mut leave = fetch_leave_from_bamboo(bamboo_company_domain, bamboo_api_key, date).unwrap();

    let mut leave_per_user = current_contiguous_period_per_user(&mut leave, date);

    send_to_slack(&mut leave_per_user, slack_webhook_url).unwrap();
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    date: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct LeavePeriod {
    r#type: String, // I've observed "timeOff" and "holiday", but there might be more.
    name: String,
    start: NaiveDate,
    end: NaiveDate,
}

impl LeavePeriod {
    fn includes(&self, date: NaiveDate) -> bool {
        self.start <= date && self.end >= date
    }
}

fn fetch_leave_from_bamboo(
    domain: String,
    api_key: String,
    day: NaiveDate,
) -> Result<Vec<LeavePeriod>> {
    let url = format!(
        "https://api.bamboohr.com/api/gateway.php/{}/v1/time_off/whos_out/",
        domain
    );
    let leave = ureq::get(url.as_str())
        .set("Accept", "application/json")
        .set("Authorization", &basic_auth_header(api_key.as_str(), "x"))
        .query("start", day.to_string().as_str())
        .query(
            "end",
            day.checked_add_days(LEAVE_LOOKAHEAD)
                .unwrap()
                .to_string()
                .as_str(),
        )
        .call()?
        .into_json::<Vec<LeavePeriod>>()?;

    Ok(leave)
}

/// Returns the first contiguous period of leave for each user (grouping by name).
///
/// Leave periods are adjacent if they:
/// - Occur on the same day
/// - Occur on adjacent days
/// - Occur with only a weekend in-between.
fn current_contiguous_period_per_user(
    leave: &mut [LeavePeriod],
    date: NaiveDate,
) -> Vec<LeavePeriod> {
    // Our per-user fold relies on leave periods being sorted.
    leave.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    let a = leave
        .iter_mut()
        .filter(|l| l.end >= date) // Ignore leave that has already ended
        .into_grouping_map_by(|l| l.name.to_string())
        .fold_first(|a, _, b| {
            if same_or_adjacent_workdays(a.end, b.start) {
                // Extend a to cover both a and b. From our earlier sort, we know that b.end >= a.end.
                a.end = b.end;
            }
            a
        });

    a.into_values()
        .map(|v| v.clone())
        .filter(|l| l.includes(date)) // Only include leave that is current.
        .collect_vec()
}

/// Returns true if dates are the same, are adjacent, or if they are separated by a weekend.
fn same_or_adjacent_workdays(a: NaiveDate, b: NaiveDate) -> bool {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };

    a == b || // Same day
    a.succ_opt().is_some_and(|aa| aa == b) || // Next day
    (a.weekday() == Weekday::Fri && a.checked_add_days(Days::new(3)).is_some_and(|aa| aa == b))
    // Crossing a weekend
}

fn send_to_slack(leave: &mut [LeavePeriod], url: String) -> Result<()> {
    leave.sort_by(|a, b| a.name.cmp(&b.name));

    let mut message_blocks: Vec<serde_json::Value> = Vec::new();

    let holidays: Vec<serde_json::Value> = leave
        .iter()
        .filter(|l| l.r#type == "holiday")
        .map(|l| {
            ureq::json!({
                "type": "rich_text_section",
                "elements": [
                    {
                        "type": "text",
                        "text": l.name,
                    }
                ]
            })
        })
        .collect();

    let time_off: Vec<serde_json::Value> = leave
        .iter()
        .filter(|l| l.r#type == "timeOff")
        .map(|l| {
            let mut elements: Vec<serde_json::Value> = Vec::new();

            elements.push(ureq::json!({
                "type": "text",
                "text": l.name,
                "style": {
                    "bold": true,
                }
            }));

            if l.start != l.end {
                elements.push(ureq::json!({
                    "type": "text",
                    "text": format!(
                        " (until {})",
                        l.end.succ_opt().unwrap().format("%A, %-d %B")
                    ),
                    "style": {
                        "italic": true,
                    }
                }));
            }

            ureq::json!({
                "type": "rich_text_section",
                "elements": elements,
            })
        })
        .collect();

    if !holidays.is_empty() {
        message_blocks.push(ureq::json!(
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": ":calendar: Holidays",
                    "emoji": true
                }
            }
        ));

        message_blocks.push(ureq::json!(
            {
                "type": "rich_text",
                "elements": [
                    {
                    "type": "rich_text_list",
                    "style": "bullet",
                    "elements": holidays,
                }]
            }
        ));
    }

    if !time_off.is_empty() {
        message_blocks.push(ureq::json!(
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": ":wave: On leave",
                    "emoji": true
                }
            }
        ));
        message_blocks.push(ureq::json!({
            "type": "rich_text",
            "elements": [{
                "type": "rich_text_list",
                "style": "bullet",
                "elements": time_off,
            }]
        }))
    }

    if message_blocks.is_empty() {
        message_blocks.push(ureq::json!(
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": "*Nobody is on leave today*",
                }
            }
        ))
    }

    let message = ureq::json!({
        "blocks": message_blocks,
    });

    ureq::post(&url).send_json(message)?;

    Ok(())
}

fn require_from_env(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("missing required environment variable: {}", key))
}

fn basic_auth_header(username: &str, password: &str) -> String {
    "Basic ".to_string()
        + base64::prelude::BASE64_STANDARD
            .encode(format!("{}:{}", username, password))
            .as_str()
}
