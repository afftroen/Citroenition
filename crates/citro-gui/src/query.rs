//! Filtering and sorting of profile rows. Shared by the backends (server side) and by the profile table, which
//! filters the cached rows locally so typing in the search box feels instant.

use crate::backend::{Query, Row, RunState, SortKey, StatusFilter, group_label, os_label, proxy_label};
use citro_core::profile::Group;
use std::cmp::Ordering;

fn norm_group(id: &str) -> &str {
    if id.is_empty() { "0" } else { id }
}

fn status_matches(f: StatusFilter, run: &RunState) -> bool {
    match f {
        StatusFilter::All => true,
        StatusFilter::Open => matches!(run, RunState::Open { .. } | RunState::Opening),
        StatusFilter::Closed => matches!(run, RunState::Closed | RunState::Closing),
        StatusFilter::Locked => matches!(run, RunState::LockedElsewhere { .. }),
    }
}

/// Lower-cased searchable text of a row: name, remark, proxy, id, serial, group, platform, country.
pub fn haystack(row: &Row, groups: &[Group]) -> String {
    let p = &row.profile;
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        p.name,
        p.remark,
        proxy_label(&p.user_proxy_config),
        p.profile_id,
        p.serial_number,
        group_label(groups, &p.group_id),
        p.domain_name,
        p.country
    )
    .to_lowercase()
}

fn status_rank(run: &RunState) -> u8 {
    match run {
        RunState::Open { .. } => 0,
        RunState::Opening => 1,
        RunState::LockedElsewhere { .. } => 2,
        RunState::Closing => 3,
        RunState::Closed => 4,
    }
}

fn ci(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

fn compare(a: &Row, b: &Row, key: SortKey, groups: &[Group]) -> Ordering {
    let (pa, pb) = (&a.profile, &b.profile);
    match key {
        SortKey::Serial => pa.serial_number.cmp(&pb.serial_number),
        SortKey::Name => ci(&pa.name, &pb.name),
        SortKey::Group => ci(&group_label(groups, &pa.group_id), &group_label(groups, &pb.group_id)),
        SortKey::Remark => ci(&pa.remark, &pb.remark),
        SortKey::Proxy => ci(&proxy_label(&pa.user_proxy_config), &proxy_label(&pb.user_proxy_config)),
        SortKey::Os => os_label(&pa.persona_os).cmp(os_label(&pb.persona_os)),
        SortKey::LastOpened => pa.last_open_time.cmp(&pb.last_open_time),
        SortKey::LastClosed => pa.last_close_time.cmp(&pb.last_close_time),
        SortKey::Created => pa.created_time.cmp(&pb.created_time),
        SortKey::Status => status_rank(&a.run).cmp(&status_rank(&b.run)),
    }
}

/// Indices into `rows` that match `q`, in the requested order. Ties always fall back to the serial number
/// ascending, so the order is stable and predictable.
pub fn view_indices(rows: &[Row], q: &Query, groups: &[Group]) -> Vec<usize> {
    let tokens: Vec<String> = q.q.split_whitespace().map(str::to_lowercase).collect();
    let mut out: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            q.group.as_deref().is_none_or(|g| norm_group(g) == norm_group(&r.profile.group_id))
                && q.team.as_deref().is_none_or(|t| r.team.as_deref() == Some(t))
                && status_matches(q.status, &r.run)
                && (tokens.is_empty() || {
                    let hay = haystack(r, groups);
                    tokens.iter().all(|t| hay.contains(t.as_str()))
                })
        })
        .map(|(i, _)| i)
        .collect();
    out.sort_by(|&a, &b| {
        let mut o = compare(&rows[a], &rows[b], q.sort, groups);
        if q.desc {
            o = o.reverse();
        }
        o.then_with(|| rows[a].profile.serial_number.cmp(&rows[b].profile.serial_number))
    });
    out
}

/// [`view_indices`] but returning owned rows (what a backend's `list_profiles` needs).
pub fn apply(rows: Vec<Row>, q: &Query, groups: &[Group]) -> Vec<Row> {
    let idx = view_indices(&rows, q, groups);
    idx.into_iter().map(|i| rows[i].clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use citro_core::profile::{Profile, ProxyConfig};

    fn groups() -> Vec<Group> {
        vec![
            Group { group_id: "1".into(), group_name: "Vinted FR".into(), remark: String::new(), ..Default::default() },
            Group { group_id: "2".into(), group_name: "eBay DE".into(), remark: String::new(), ..Default::default() },
        ]
    }

    fn row(serial: u64, name: &str, group: &str, remark: &str, proxy: Option<&str>, run: RunState) -> Row {
        let mut p = Profile {
            profile_id: format!("k{serial:03}"),
            serial_number: serial,
            name: name.into(),
            group_id: group.into(),
            remark: remark.into(),
            ..Default::default()
        };
        if let Some(u) = proxy {
            p.user_proxy_config = ProxyConfig::from_url(u).unwrap();
        }
        Row { profile: p, run, team: None }
    }

    fn locked() -> RunState {
        RunState::LockedElsewhere { user: "mia".into(), device: "MIA-LAPTOP".into(), since: 1 }
    }

    fn sample() -> Vec<Row> {
        vec![
            row(1, "Amélie Dubois", "1", "warm-up day 3", Some("socks5://203.0.113.10:1080"), RunState::Closed),
            row(
                2,
                "lukas schneider",
                "2",
                "Versand ab Köln",
                Some("http://198.51.100.7:8080"),
                RunState::Open { pid: 7, since: 5 },
            ),
            row(3, "Oliver Hughes", "0", "", None, locked()),
            row(
                4,
                "Chloé Martin",
                "1",
                "needs SMS",
                Some("socks5://203.0.113.11:1081"),
                RunState::Open { pid: 8, since: 6 },
            ),
            row(5, "Zoé", "", "last", None, RunState::Closed),
        ]
    }

    fn serials(rows: &[Row], idx: &[usize]) -> Vec<u64> {
        idx.iter().map(|&i| rows[i].profile.serial_number).collect()
    }

    #[test]
    fn default_query_keeps_everything_in_serial_order() {
        let r = sample();
        assert_eq!(serials(&r, &view_indices(&r, &Query::default(), &groups())), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn group_filter_treats_empty_as_ungrouped() {
        let r = sample();
        let q = |g: &str| Query { group: Some(g.into()), ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q("1"), &groups())), [1, 4]);
        assert_eq!(serials(&r, &view_indices(&r, &q("0"), &groups())), [3, 5]);
        assert_eq!(serials(&r, &view_indices(&r, &q("9"), &groups())), Vec::<u64>::new());
    }

    #[test]
    fn status_filter() {
        let r = sample();
        let q = |s| Query { status: s, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q(StatusFilter::Open), &groups())), [2, 4]);
        assert_eq!(serials(&r, &view_indices(&r, &q(StatusFilter::Closed), &groups())), [1, 5]);
        assert_eq!(serials(&r, &view_indices(&r, &q(StatusFilter::Locked), &groups())), [3]);
        assert_eq!(serials(&r, &view_indices(&r, &q(StatusFilter::All), &groups())).len(), 5);
    }

    #[test]
    fn text_search_is_case_insensitive_and_and_ed() {
        let r = sample();
        let q = |t: &str| Query { q: t.into(), ..Default::default() };
        let run = |t: &str| serials(&r, &view_indices(&r, &q(t), &groups()));
        assert_eq!(run("LUKAS"), [2]); // name
        assert_eq!(run("sms"), [4]); // remark
        assert_eq!(run("203.0.113"), [1, 4]); // proxy host
        assert_eq!(run("socks5 1081"), [4]); // two tokens, both must match
        assert_eq!(run("k003"), [3]); // profile id
        assert_eq!(run("vinted"), [1, 4]); // group name
        assert_eq!(run("direct"), [3, 5]); // no proxy
        assert_eq!(run("  "), [1, 2, 3, 4, 5]);
        assert_eq!(run("nomatch"), Vec::<u64>::new());
        assert_eq!(run("é"), [1, 4, 5]); // accented letters match as typed
    }

    #[test]
    fn sorting_by_name_is_case_insensitive_and_reversible() {
        let r = sample();
        let q = Query { sort: SortKey::Name, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [1, 4, 2, 3, 5]);
        let q = Query { sort: SortKey::Name, desc: true, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [5, 3, 2, 4, 1]);
    }

    #[test]
    fn sorting_by_status_puts_open_first_then_locked() {
        let r = sample();
        let q = Query { sort: SortKey::Status, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [2, 4, 3, 1, 5]);
    }

    #[test]
    fn sorting_by_dates_with_never_and_serial_tiebreak() {
        let mut r = sample();
        r[0].profile.last_open_time = 500;
        r[1].profile.last_open_time = 900;
        r[3].profile.last_open_time = 500; // ties with #1 -> serial order
        let asc = Query { sort: SortKey::LastOpened, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &asc, &groups())), [3, 5, 1, 4, 2]);
        let desc = Query { sort: SortKey::LastOpened, desc: true, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &desc, &groups())), [2, 1, 4, 3, 5]);
    }

    #[test]
    fn sorting_by_group_uses_names_not_ids() {
        let r = sample();
        let q = Query { sort: SortKey::Group, ..Default::default() };
        // eBay DE (2), Ungrouped (3, 5), Vinted FR (1, 4)
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [2, 3, 5, 1, 4]);
    }

    #[test]
    fn filters_combine_and_team_scoping_works() {
        let mut r = sample();
        r[0].team = Some("t1".into());
        r[3].team = Some("t1".into());
        let q = Query { group: Some("1".into()), status: StatusFilter::Open, ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [4]);
        let q = Query { team: Some("t1".into()), ..Default::default() };
        assert_eq!(serials(&r, &view_indices(&r, &q, &groups())), [1, 4]);
        assert_eq!(apply(r, &q, &groups()).len(), 2);
    }
}
