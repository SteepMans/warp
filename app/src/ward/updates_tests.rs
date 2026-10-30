use chrono::{Duration, TimeZone as _};

use super::*;
use crate::ward::changelog::{ChangelogGroup, ChangelogNote};

fn version(version: &str) -> VersionInfo {
    VersionInfo::new(version.to_owned())
}

fn now() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
}

fn checked() -> CheckInfo<'static> {
    CheckInfo {
        checked: true,
        polling_started: true,
        ..Default::default()
    }
}

/// Every stage, so a new variant breaks the exhaustive match in `status` and shows up here.
fn all_stages() -> Vec<AutoupdateStage> {
    vec![
        AutoupdateStage::NoUpdateAvailable,
        AutoupdateStage::CheckingForUpdate,
        AutoupdateStage::DownloadingUpdate,
        AutoupdateStage::UnableToUpdateToNewVersion {
            new_version: version("v2"),
        },
        AutoupdateStage::UpdateReady {
            new_version: version("v2"),
            update_id: "id".to_owned(),
        },
        AutoupdateStage::Updating {
            new_version: version("v2"),
            update_id: "id".to_owned(),
        },
        AutoupdateStage::UnableToLaunchNewVersion {
            new_version: version("v2"),
        },
        AutoupdateStage::UpdatedPendingRestart {
            new_version: version("v2"),
        },
    ]
}

#[test]
fn no_update_after_a_check_is_up_to_date() {
    let status = status(&AutoupdateStage::NoUpdateAvailable, checked());
    assert_eq!(status.text, "Up to date");
    assert_eq!(status.kind, StatusKind::Idle);
    assert_eq!(status.action, None);
}

#[test]
fn failed_check_shows_the_reason_and_retry() {
    let info = CheckInfo {
        error: Some("connection refused"),
        ..checked()
    };
    let status = status(&AutoupdateStage::NoUpdateAvailable, info);
    assert_eq!(status.text, "Error: connection refused");
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(status.action, Some(StatusAction::Retry));
}

#[test]
fn before_the_first_check_says_so() {
    let waiting = status(&AutoupdateStage::NoUpdateAvailable, CheckInfo::default());
    assert_eq!(
        waiting.text,
        "Update checks start once Ward finishes launching"
    );
    let info = CheckInfo {
        polling_started: true,
        ..Default::default()
    };
    assert_eq!(
        status(&AutoupdateStage::NoUpdateAvailable, info).text,
        "Not checked yet"
    );
}

#[test]
fn checking_and_downloading_are_busy_without_actions() {
    let checking = status(&AutoupdateStage::CheckingForUpdate, checked());
    assert_eq!(checking.text, "Checking for updates…");
    assert_eq!(checking.kind, StatusKind::Busy);
    assert_eq!(checking.action, None);

    let info = CheckInfo {
        latest_version: Some("v0.2026.10.30.08.37.oss_00"),
        ..checked()
    };
    let downloading = status(&AutoupdateStage::DownloadingUpdate, info);
    assert_eq!(downloading.text, "Downloading v0.2026.10.30.08.37.oss_00…");
    assert_eq!(downloading.kind, StatusKind::Busy);
    assert_eq!(
        status(&AutoupdateStage::DownloadingUpdate, checked()).text,
        "Downloading the update…"
    );
}

#[test]
fn ready_update_offers_install_and_restart() {
    let stage = AutoupdateStage::UpdateReady {
        new_version: version("v0.2026.10.30.08.37.oss_00"),
        update_id: "id".to_owned(),
    };
    let status = status(&stage, checked());
    assert_eq!(status.text, "Ready to install: v0.2026.10.30.08.37.oss_00");
    assert_eq!(status.kind, StatusKind::Ready);
    assert_eq!(status.action, Some(StatusAction::InstallAndRestart));
    assert_eq!(
        StatusAction::InstallAndRestart.label(),
        "Install and restart"
    );
}

#[test]
fn installed_update_offers_restart() {
    let stage = AutoupdateStage::UpdatedPendingRestart {
        new_version: version("v2"),
    };
    let status = status(&stage, checked());
    assert_eq!(status.text, "v2 is installed, restart Ward to use it");
    assert_eq!(status.action, Some(StatusAction::Restart));
}

#[test]
fn failed_relaunch_and_missing_permission_offer_retry() {
    let failed = status(
        &AutoupdateStage::UnableToLaunchNewVersion {
            new_version: version("v2"),
        },
        checked(),
    );
    assert_eq!(failed.text, "Error: could not restart into v2");
    assert_eq!(failed.action, Some(StatusAction::Retry));

    let no_permission = status(
        &AutoupdateStage::UnableToUpdateToNewVersion {
            new_version: version("v2"),
        },
        checked(),
    );
    assert_eq!(no_permission.kind, StatusKind::Error);
    assert_eq!(no_permission.action, Some(StatusAction::Retry));
}

#[test]
fn every_stage_has_a_status_and_a_check_now_decision() {
    for stage in all_stages() {
        assert!(!status(&stage, checked()).text.is_empty(), "{stage:?}");
        let _ = decide_check_now(&stage, None, now());
    }
}

#[test]
fn check_now_never_starts_a_second_check_while_one_runs() {
    for stage in [
        AutoupdateStage::CheckingForUpdate,
        AutoupdateStage::DownloadingUpdate,
        AutoupdateStage::Updating {
            new_version: version("v2"),
            update_id: "id".to_owned(),
        },
    ] {
        assert_eq!(
            decide_check_now(&stage, None, now()),
            CheckNowDecision::InProgress,
            "{stage:?}"
        );
    }
    let pending = AutoupdateStage::UpdatedPendingRestart {
        new_version: version("v2"),
    };
    assert_eq!(
        decide_check_now(&pending, None, now()),
        CheckNowDecision::RestartPending
    );
}

#[test]
fn check_now_skips_a_check_that_just_finished() {
    let stage = AutoupdateStage::NoUpdateAvailable;
    let just_now = now() - Duration::seconds(2);
    assert_eq!(
        decide_check_now(&stage, Some(just_now), now()),
        CheckNowDecision::RecentlyChecked
    );
    let a_while_ago = now() - Duration::seconds(RECENT_CHECK_SECONDS);
    assert_eq!(
        decide_check_now(&stage, Some(a_while_ago), now()),
        CheckNowDecision::Start
    );
    assert_eq!(
        decide_check_now(&stage, None, now()),
        CheckNowDecision::Start
    );
    // A clock that went back does not block checks.
    let future = now() + Duration::minutes(10);
    assert_eq!(
        decide_check_now(&stage, Some(future), now()),
        CheckNowDecision::Start
    );
}

#[test]
fn check_now_checks_again_after_an_error_or_with_a_ready_update() {
    let old = Some(now() - Duration::minutes(5));
    for stage in [
        AutoupdateStage::NoUpdateAvailable,
        AutoupdateStage::UpdateReady {
            new_version: version("v2"),
            update_id: "id".to_owned(),
        },
        AutoupdateStage::UnableToLaunchNewVersion {
            new_version: version("v2"),
        },
    ] {
        assert_eq!(
            decide_check_now(&stage, old, now()),
            CheckNowDecision::Start,
            "{stage:?}"
        );
    }
}

#[test]
fn every_skipped_check_explains_itself() {
    for decision in [
        CheckNowDecision::InProgress,
        CheckNowDecision::RestartPending,
        CheckNowDecision::RecentlyChecked,
    ] {
        assert!(decision.note(true).is_some(), "{decision:?}");
    }
    assert_eq!(CheckNowDecision::Start.note(true), None);
    assert!(CheckNowDecision::Start.note(false).is_some());
}

#[test]
fn last_checked_text_uses_minutes_hours_and_dates() {
    assert_eq!(last_checked_text(None, now()), "Not checked since launch");
    assert_eq!(
        last_checked_text(Some(now() - Duration::seconds(20)), now()),
        "Last checked just now"
    );
    assert_eq!(
        last_checked_text(Some(now() - Duration::minutes(3)), now()),
        "Last checked 3 min ago"
    );
    assert_eq!(
        last_checked_text(Some(now() - Duration::hours(2)), now()),
        "Last checked 2 h ago"
    );
    assert_eq!(
        last_checked_text(Some(now() - Duration::days(2)), now()),
        "Last checked on 2026-10-28 12:00"
    );
}

#[test]
fn notes_are_for_the_latest_version_or_the_running_one() {
    let latest = version("v2");
    assert_eq!(notes_version(Some(&latest), Some("v1")), Some("v2"));
    assert_eq!(notes_version(None, Some("v1")), Some("v1"));
    assert_eq!(notes_version(None, None), None);
}

#[test]
fn short_notes_lists_first_notes_and_counts_the_rest() {
    let note = |section: &str, title: &str| ChangelogNote {
        section: section.to_owned(),
        title: title.to_owned(),
        text: format!("{title} text"),
        image_url: None,
    };
    let mut notes: Vec<ChangelogNote> = (0..MAX_NOTES + 2)
        .map(|index| note("improved", &format!("Note {index}")))
        .collect();
    notes[0] = note("new", "Updates page");
    notes.push(ChangelogNote::default());
    let changelog = WardChangelog {
        notes,
        ..Default::default()
    };
    let (lines, hidden) = short_notes(&changelog);
    assert_eq!(lines.len(), MAX_NOTES);
    assert_eq!(hidden, 2);
    assert_eq!(lines[0].section, crate::ward::changelog::NEW_SECTION_TITLE);
    assert_eq!(lines[0].title, "Updates page");
    assert_eq!(lines[0].text, "Updates page text");
    assert_eq!(
        lines[1].section,
        crate::ward::changelog::IMPROVED_SECTION_TITLE
    );
}

#[test]
fn short_notes_falls_back_to_commit_groups() {
    let changelog = WardChangelog {
        groups: vec![
            ChangelogGroup {
                module: "Tabs".to_owned(),
                items: vec!["filter".to_owned(), " ".to_owned(), "counters".to_owned()],
            },
            ChangelogGroup {
                module: "Empty".to_owned(),
                items: vec![" ".to_owned()],
            },
        ],
        ..Default::default()
    };
    let (lines, hidden) = short_notes(&changelog);
    assert_eq!(hidden, 0);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].title, "Tabs");
    assert_eq!(lines[0].text, "filter; counters");
}

#[test]
fn release_details_combine_date_and_short_commit() {
    let changelog = WardChangelog {
        date: Some("2026-10-29T12:00:00Z".to_owned()),
        commit: Some("18c063137abcdef".to_owned()),
        ..Default::default()
    };
    let details = release_details(&changelog).unwrap();
    assert!(details.starts_with("Released 2026-10-29 "), "{details}");
    assert!(details.ends_with(" · commit 18c0631"), "{details}");

    let commit_only = WardChangelog {
        commit: Some("abc".to_owned()),
        ..Default::default()
    };
    assert_eq!(release_details(&commit_only).as_deref(), Some("commit abc"));
    assert_eq!(release_details(&WardChangelog::default()), None);
}
