use super::*;

#[test]
fn forces_short_polling_for_verbose_system_commands() {
    let delay = force_short_polling_delay_for_command(
        "journalctl --since yesterday | grep -i error",
        Some(ShellCommandDelay::OnCompletion),
    );

    assert_eq!(
        delay,
        Some(ShellCommandDelay::Duration(FORCED_SHORT_POLL_DURATION))
    );
}

#[test]
fn caps_existing_delay_for_high_risk_commands() {
    let delay = force_short_polling_delay_for_command(
        "launchctl print gui/501",
        Some(ShellCommandDelay::Duration(Duration::from_secs(60))),
    );

    assert_eq!(
        delay,
        Some(ShellCommandDelay::Duration(FORCED_SHORT_POLL_DURATION))
    );
}

#[test]
fn keeps_shorter_existing_delay_for_high_risk_commands() {
    let delay = force_short_polling_delay_for_command(
        "find /etc -name '*.plist'",
        Some(ShellCommandDelay::Duration(Duration::from_secs(1))),
    );

    assert_eq!(
        delay,
        Some(ShellCommandDelay::Duration(Duration::from_secs(1)))
    );
}

#[test]
fn leaves_normal_commands_unchanged() {
    let delay = force_short_polling_delay_for_command(
        "cargo check -p warp",
        Some(ShellCommandDelay::OnCompletion),
    );

    assert_eq!(delay, Some(ShellCommandDelay::OnCompletion));
}
