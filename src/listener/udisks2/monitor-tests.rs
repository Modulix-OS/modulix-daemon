use super::*;

#[test]
fn name_is_udisks2_monitor() {
    assert_eq!(Udisks2MonitorListener.name(), "udisks2-monitor");
}

/// The rules are the contract handed to `BecomeMonitor`: without the two reply
/// rules the listener could not tell an authorised call from one polkit
/// refused, and would report both.
#[test]
fn match_rules_cover_the_calls_and_their_replies() {
    assert_eq!(
        match_rules().unwrap(),
        vec![
            "type='method_call',interface='org.freedesktop.UDisks2.Block'".to_string(),
            "type='method_return',sender='org.freedesktop.UDisks2'".to_string(),
            "type='error',sender='org.freedesktop.UDisks2'".to_string(),
        ]
    );
}
