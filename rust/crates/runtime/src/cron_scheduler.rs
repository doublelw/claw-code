use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minute: Vec<u8>,       // 0-59
    hour: Vec<u8>,         // 0-23
    day_of_month: Vec<u8>, // 1-31
    month: Vec<u8>,        // 1-12
    day_of_week: Vec<u8>,  // 0-6 (0=Sunday)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronEntry {
    pub id: String,
    pub expression: String,
    pub prompt: String,
    pub recurring: bool,
    pub schedule: CronSchedule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronParseError {
    InvalidFieldCount,
    InvalidMinute(String),
    InvalidHour(String),
    InvalidDayOfMonth(String),
    InvalidMonth(String),
    InvalidDayOfWeek(String),
}

impl std::fmt::Display for CronParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFieldCount => write!(f, "cron expression must have 5 fields"),
            Self::InvalidMinute(s) => write!(f, "invalid minute: {s}"),
            Self::InvalidHour(s) => write!(f, "invalid hour: {s}"),
            Self::InvalidDayOfMonth(s) => write!(f, "invalid day of month: {s}"),
            Self::InvalidMonth(s) => write!(f, "invalid month: {s}"),
            Self::InvalidDayOfWeek(s) => write!(f, "invalid day of week: {s}"),
        }
    }
}

impl std::error::Error for CronParseError {}

pub fn parse_cron_expression(expr: &str) -> Result<CronSchedule, CronParseError> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CronParseError::InvalidFieldCount);
    }

    Ok(CronSchedule {
        minute: parse_field(fields[0], 0, 59).map_err(CronParseError::InvalidMinute)?,
        hour: parse_field(fields[1], 0, 23).map_err(CronParseError::InvalidHour)?,
        day_of_month: parse_field(fields[2], 1, 31).map_err(CronParseError::InvalidDayOfMonth)?,
        month: parse_field(fields[3], 1, 12).map_err(CronParseError::InvalidMonth)?,
        day_of_week: parse_field(fields[4], 0, 6).map_err(CronParseError::InvalidDayOfWeek)?,
    })
}

fn parse_field(field: &str, min: u8, max: u8) -> Result<Vec<u8>, String> {
    if field == "*" {
        return Ok((min..=max).collect());
    }

    let mut values = Vec::new();
    for part in field.split(',') {
        if let Some((start_str, end_str)) = part.split_once('-') {
            let start: u8 = start_str.parse().map_err(|_| part.to_string())?;
            let end: u8 = end_str.parse().map_err(|_| part.to_string())?;
            for v in start..=end {
                if v < min || v > max {
                    return Err(part.to_string());
                }
                values.push(v);
            }
        } else if let Some(step_str) = part.strip_prefix("*/") {
            let step: u8 = step_str.parse().map_err(|_| part.to_string())?;
            if step == 0 {
                return Err(part.to_string());
            }
            let mut v = min;
            while v <= max {
                values.push(v);
                v = v.saturating_add(step);
                if step == 0 {
                    break;
                }
            }
        } else {
            let v: u8 = part.parse().map_err(|_| part.to_string())?;
            if v < min || v > max {
                return Err(part.to_string());
            }
            values.push(v);
        }
    }

    values.sort();
    values.dedup();
    Ok(values)
}

pub fn next_fire_time(schedule: &CronSchedule, after: SystemTime) -> SystemTime {
    let secs = after
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Check every minute from after+60s
    let start_sec = (secs / 60 + 1) * 60;
    for offset in 0..525600 {
        let candidate = start_sec + offset * 60;
        let days_since_epoch = (candidate / 86400) as i32;
        let day_of_week = ((days_since_epoch + 4) % 7) as u8; // 1970-01-01 was Thursday
        let remaining = candidate % 86400;
        let hour = (remaining / 3600) as u8;
        let minute = ((remaining % 3600) / 60) as u8;

        // Simplified month/day check
        if schedule.minute.contains(&minute)
            && schedule.hour.contains(&hour)
            && schedule.day_of_week.contains(&day_of_week)
        {
            return UNIX_EPOCH + Duration::from_secs(candidate);
        }
    }
    after
}

pub struct CronScheduler {
    entries: HashMap<String, ScheduledEntry>,
}

struct ScheduledEntry {
    config: CronEntry,
    next_fire: Option<SystemTime>,
}

impl CronScheduler {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn add(&mut self, entry: CronEntry) {
        let next_fire = Some(next_fire_time(&entry.schedule, SystemTime::now()));
        self.entries.insert(
            entry.id.clone(),
            ScheduledEntry {
                config: entry,
                next_fire,
            },
        );
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.entries.remove(id).is_some()
    }

    pub fn due_entries(&mut self) -> Vec<CronEntry> {
        let now = SystemTime::now();
        let mut due = Vec::new();
        for (_, entry) in &mut self.entries {
            if let Some(next) = entry.next_fire {
                if next <= now {
                    due.push(entry.config.clone());
                    entry.next_fire = Some(next_fire_time(&entry.config.schedule, now));
                }
            }
        }
        due
    }

    pub fn list(&self) -> Vec<&CronEntry> {
        self.entries.values().map(|e| &e.config).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_star_all_fields() {
        let schedule = parse_cron_expression("* * * * *").unwrap();
        assert_eq!(schedule.minute.len(), 60);
        assert_eq!(schedule.hour.len(), 24);
    }

    #[test]
    fn parse_specific_values() {
        let schedule = parse_cron_expression("0 12 * * 1").unwrap();
        assert_eq!(schedule.minute, vec![0]);
        assert_eq!(schedule.hour, vec![12]);
        assert_eq!(schedule.day_of_week, vec![1]);
    }

    #[test]
    fn parse_step_expression() {
        let schedule = parse_cron_expression("*/5 * * * *").unwrap();
        assert_eq!(
            schedule.minute,
            vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]
        );
    }

    #[test]
    fn parse_range_expression() {
        let schedule = parse_cron_expression("1-5 * * * *").unwrap();
        assert_eq!(schedule.minute, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn parse_comma_expression() {
        let schedule = parse_cron_expression("1,15 * * * *").unwrap();
        assert_eq!(schedule.minute, vec![1, 15]);
    }

    #[test]
    fn reject_wrong_field_count() {
        assert!(matches!(
            parse_cron_expression("* * *"),
            Err(CronParseError::InvalidFieldCount)
        ));
        assert!(matches!(
            parse_cron_expression("* * * * * *"),
            Err(CronParseError::InvalidFieldCount)
        ));
    }

    #[test]
    fn reject_out_of_range() {
        assert!(parse_cron_expression("60 * * * *").is_err());
        assert!(parse_cron_expression("* 24 * * *").is_err());
    }

    #[test]
    fn scheduler_add_remove() {
        let mut scheduler = CronScheduler::new();
        let entry = CronEntry {
            id: "test".to_string(),
            expression: "*/5 * * * *".to_string(),
            prompt: "hello".to_string(),
            recurring: true,
            schedule: parse_cron_expression("*/5 * * * *").unwrap(),
        };
        scheduler.add(entry);
        assert_eq!(scheduler.list().len(), 1);
        assert!(scheduler.remove("test"));
        assert_eq!(scheduler.list().len(), 0);
    }

    #[test]
    fn scheduler_remove_nonexistent() {
        let mut scheduler = CronScheduler::new();
        assert!(!scheduler.remove("nonexistent"));
    }

    #[test]
    fn next_fire_time_returns_future() {
        let schedule = parse_cron_expression("0 * * * *").unwrap();
        let now = SystemTime::now();
        let next = next_fire_time(&schedule, now);
        assert!(next > now);
    }
}
