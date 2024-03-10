use anyhow::Result;
use base64::Engine;
use chrono::{Datelike, Days, NaiveDate, Weekday};
use clap::Parser;
use itertools::Itertools;
use serde::Deserialize;
use ureq::OrAnyStatus;

use std::env;

// If you take more than a year of leave, we might miss it. Sorry.
const LEAVE_LOOKAHEAD: Days = Days::new(365);

const PEEK_BAMBOO_RESPONSE: bool = true;

fn main() {
    let args: Args = Args::parse();

    let bamboo_company_domain = require_from_env("BAMBOO_COMPANY_DOMAIN");
    let bamboo_api_key = require_from_env("BAMBOO_API_KEY");
    let slack_webhook_url = require_from_env("SLACK_WEBHOOK_URL");

    let date = match args.date {
        Some(date) => chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .expect("Invalid date argument (expected YYYY-MM-DD)"),
        None => chrono::Local::now().date_naive(),
    };

    println!("sending leave for {}", date);

    let leave = fetch_leave_from_bamboo(&bamboo_company_domain, &bamboo_api_key, date).unwrap();

    let mut current_holidays: Vec<Holiday> = leave
        .iter()
        .filter_map(|l| match l {
            Leave::Holiday(h) if h.includes(date) => Some(h.clone()),
            _ => None,
        })
        .collect();

    let mut time_off: Vec<TimeOff> = leave
        .iter()
        .filter_map(|l| match l {
            Leave::TimeOff(t) => Some(t.clone()),
            _ => None,
        })
        .collect();

    let leave_per_user = current_contiguous_period_per_user(&mut time_off, date);

    let mut leave_with_user_info: Vec<TimeOffWithEmployeeInfo> = leave_per_user
        .iter()
        .map(|t| get_employee_info(&bamboo_company_domain, &bamboo_api_key, t.clone()).unwrap())
        .collect();

    send_to_slack(
        &mut current_holidays,
        &mut leave_with_user_info,
        slack_webhook_url,
        date,
    )
    .unwrap();
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    date: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct Holiday {
    name: String,
    start: NaiveDate,
    end: NaiveDate,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct TimeOff {
    #[allow(non_snake_case)]
    employee_id: usize,
    name: String,
    start: NaiveDate,
    end: NaiveDate,
}

#[derive(Debug)]
struct TimeOffWithEmployeeInfo {
    time_off: TimeOff,
    employee_info: Option<EmployeeInfo>,
}

impl TimeOffWithEmployeeInfo {
    fn first_name_from_time_off(&self) -> String {
        self.time_off
            .name
            .split(' ')
            .next()
            .unwrap_or("")
            .to_string()
    }

    fn first_display_name(&self) -> String {
        if let Some(info) = &self.employee_info {
            info.preferred_name
                .clone()
                .or(info.first_name.clone())
                .unwrap_or(self.first_name_from_time_off())
        } else {
            self.first_name_from_time_off()
        }
    }

    fn last_display_name(&self) -> String {
        self.employee_info
            .as_ref()
            .and_then(|i| i.last_name.clone())
            .unwrap_or(self.time_off.name.split(' ').skip(1).join(" "))
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Leave {
    Holiday(Holiday),
    TimeOff(TimeOff),
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl TimeOff {
    fn includes(&self, date: NaiveDate) -> bool {
        self.start <= date && self.end >= date
    }

    fn return_date(&self) -> NaiveDate {
        if self.end.weekday().num_days_from_monday() >= Weekday::Fri.num_days_from_monday() {
            self.end
                .checked_add_days(Days::new(
                    (7 - self.end.weekday().num_days_from_monday()).into(),
                ))
                .unwrap()
        } else {
            self.end.checked_add_days(Days::new(1)).unwrap()
        }
    }
}

impl Holiday {
    fn includes(&self, date: NaiveDate) -> bool {
        self.start <= date && self.end >= date
    }
}

fn fetch_leave_from_bamboo(domain: &str, api_key: &str, day: NaiveDate) -> Result<Vec<Leave>> {
    let url = format!(
        "https://api.bamboohr.com/api/gateway.php/{}/v1/time_off/whos_out/",
        domain
    );
    let resp = ureq::get(url.as_str())
        .set("Accept", "application/json")
        .set("Authorization", &basic_auth_header(api_key, "x"))
        .query("start", day.to_string().as_str())
        .query(
            "end",
            day.checked_add_days(LEAVE_LOOKAHEAD)
                .unwrap()
                .to_string()
                .as_str(),
        )
        .call()?;

    let leave = if PEEK_BAMBOO_RESPONSE {
        let body = resp.into_json::<serde_json::Value>()?;
        println!(
            "\nResponse from BambooHR\n{}\n",
            serde_json::to_string_pretty(&body)?
        );

        println!("{:?}", serde_json::from_value::<Vec<Leave>>(body.clone())?);
        serde_json::from_value::<Vec<Leave>>(body)?
    } else {
        resp.into_json::<Vec<Leave>>()?
    };

    Ok(leave)
}

/// Returns the first contiguous period of leave for each user (grouping by name).
///
/// Leave periods are adjacent if they:
/// - Occur on the same day
/// - Occur on adjacent days
/// - Occur with only a weekend in-between.
fn current_contiguous_period_per_user(leave: &mut [TimeOff], date: NaiveDate) -> Vec<TimeOff> {
    // Our per-user fold relies on leave periods being sorted.
    leave.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    let a = leave
        .iter_mut()
        .filter(|l| l.end >= date) // Ignore leave that has already ended
        .into_grouping_map_by(|l| l.employee_id.to_string())
        .fold_first(|a, _, b: &mut TimeOff| {
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

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct EmployeeInfo {
    first_name: Option<String>,
    last_name: Option<String>,
    preferred_name: Option<String>,
}

fn get_employee_info(
    domain: &str,
    api_key: &str,
    time_off: TimeOff,
) -> Result<TimeOffWithEmployeeInfo> {
    let url = format!(
        "https://api.bamboohr.com/api/gateway.php/{}/v1/employees/{}/",
        domain, time_off.employee_id,
    );

    let resp = ureq::get(url.as_str())
        .set("Accept", "application/json")
        .set("Authorization", &basic_auth_header(api_key, "x"))
        .query("fields", "firstName,lastName,preferredName")
        .call();

    match resp {
        Ok(resp) => {
            let employee_info = resp.into_json::<EmployeeInfo>()?;
            Ok(TimeOffWithEmployeeInfo {
                employee_info: Some(employee_info),
                time_off,
            })
        }
        Err(ureq::Error::Status(status, _)) => {
            println!("warning: {} on employee info fetch", status);
            Ok(TimeOffWithEmployeeInfo {
                employee_info: None,
                time_off,
            })
        }
        Err(e) => Err(e.into()),
    }
}

fn send_to_slack(
    holidays: &mut [Holiday],
    time_off: &mut [TimeOffWithEmployeeInfo],
    url: String,
    date: NaiveDate,
) -> Result<()> {
    holidays.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(a.end.cmp(&b.end))
            .then(a.name.cmp(&b.name))
    });
    time_off.sort_by(|a, b| {
        a.last_display_name()
            .cmp(&b.last_display_name())
            .then(a.time_off.employee_id.cmp(&b.time_off.employee_id))
    });

    let mut message_blocks: Vec<serde_json::Value> = Vec::new();

    let holidays: Vec<serde_json::Value> = holidays
        .iter()
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

    let time_off: Vec<serde_json::Value> = time_off
        .iter()
        .map(|l| {
            let mut elements: Vec<serde_json::Value> = Vec::new();

            elements.push(ureq::json!({
                "type": "text",
                "text": l.first_display_name() + " ",
            }));

            elements.push(ureq::json!({
                "type": "text",
                "text": l.last_display_name(),
                "style": {
                    "bold": true,
                }
            }));

            elements.push(ureq::json!({
                "type": "text",
                "text": format!(
                    "   (until {})",
                    {
                        let back = l.time_off.return_date();

                        if date.succ_opt().unwrap() == back {
                            "tomorrow".to_string()
                        } else if date.checked_add_days(Days::new(7)).unwrap() > back {
                            // This coming week - use the name of the day of the week.
                            back.format("%A").to_string()
                        } else if date.checked_add_days(Days::new(7)).unwrap() == back {
                            // 7 days away, use the date but with "next".
                            back.format("next %A").to_string()
                        } else {
                            // Further ahead - use the date.
                            back.format("%-d %B").to_string()
                        }
                    }
                ),
                "style": {
                    "italic": true,
                }
            }));

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

    let resp = ureq::post(&url).send_json(&message).or_any_status()?;

    if resp.status() >= 400 {
        println!(
            "Warning: slack request failed (status {})",
            resp.status_text()
        );
        println!("request\n{}\n", serde_json::to_string_pretty(&message)?);
        println!("response\n{}\n", resp.into_string()?);
        return Err(anyhow::format_err!("request to Slack API failed"));
    }

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
