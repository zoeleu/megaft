//! Parsers against trimmed copies of real intra pages.

use megaft_core::{parse, slots};

const SLOTS_PAGE: &str = include_str!("fixtures/slots_page.html");
const HOME: &str = include_str!("fixtures/home.html");
const MINE: &str = include_str!("fixtures/mine.html");
const SLOTS_JSON: &str = include_str!("fixtures/slots.json");

#[test]
fn calendar_attributes() {
    let cal = parse::calendar(SLOTS_PAGE).unwrap().unwrap();
    assert_eq!(cal.team_id, "7000001");
    assert_eq!(cal.duration, 2);
    assert_eq!(cal.index_url, "/projects/codexion/slots.json?team_id=7000001");
    assert_eq!(
        cal.update_url,
        "/projects/codexion/slots/:ids.json?team_id=7000001"
    );
    assert_eq!(cal.csrf_token.as_deref(), Some("TEST-CSRF-TOKEN=="));
}

#[test]
fn no_calendar() {
    assert_eq!(parse::calendar(HOME).unwrap(), None);
}

#[test]
fn whoami_from_user_script() {
    assert_eq!(parse::whoami(SLOTS_PAGE).as_deref(), Some("student"));
    assert_eq!(parse::whoami(HOME), None);
}

#[test]
fn open_projects_with_status() {
    let p = parse::open_projects(HOME).unwrap();
    let got: Vec<_> = p
        .iter()
        .map(|p| (p.slug.as_str(), p.name.as_str(), p.status.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("42next-exam-rank-03", "Exam Rank 03", "In progress"),
            ("codexion", "Codexion", "Evaluation needed"),
            ("fly-in", "Fly-in", "In progress"),
        ]
    );
}

#[test]
fn only_evaluations_of_me() {
    let up = parse::upcoming_evaluations(MINE).unwrap();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].to_string(), "2026-11-24T02:00:00Z");
}

#[test]
fn slot_blocks_json() {
    let blocks: Vec<slots::SlotBlock> = serde_json::from_str(SLOTS_JSON).unwrap();
    let c = slots::candidates(
        &blocks,
        2,
        "2026-11-24T00:00:00Z".parse().unwrap(),
        "2026-11-25T00:00:00Z".parse().unwrap(),
        &[],
    );
    let got: Vec<_> = c
        .iter()
        .map(|c| (c.span.start.to_string(), c.ids.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("2026-11-24T12:00:00Z".to_owned(), "aaa==,bbb=="),
            ("2026-11-24T12:45:00Z".to_owned(), "ccc==,ddd=="),
            ("2026-11-24T13:00:00Z".to_owned(), "ddd==,eee=="),
        ]
    );
}

#[test]
fn cloudflare_detection() {
    assert!(parse::is_cloudflare_challenge(
        403,
        "<title>Just a moment...</title>"
    ));
    assert!(!parse::is_cloudflare_challenge(200, "Just a moment"));
    assert!(!parse::is_cloudflare_challenge(403, "Access Denied"));
}
