use vulcan_event_relay::{validate_git_event, CloudEvent, SubscriptionBundle};

#[test]
fn checked_in_subscription_fixture_is_valid_and_redacted() {
    let bundle: SubscriptionBundle =
        serde_json::from_str(include_str!("fixtures/valid-subscription.json"))
            .expect("subscription fixture");
    bundle.validate().expect("valid subscription fixture");
    let output = serde_json::to_string(&bundle).expect("redacted output");
    assert!(!output.contains(bundle.credential.token.expose_secret()));
}

#[test]
fn checked_in_git_event_fixtures_cover_acceptance_and_rejection() {
    let valid: CloudEvent = serde_json::from_str(include_str!("fixtures/valid-refs-updated.json"))
        .expect("valid event fixture");
    validate_git_event(&valid).expect("valid Git event");

    let invalid: CloudEvent =
        serde_json::from_str(include_str!("fixtures/invalid-refs-updated.json"))
            .expect("invalid event fixture");
    assert_eq!(
        validate_git_event(&invalid)
            .expect_err("invalid Git ref")
            .code,
        "git.invalid-ref"
    );
}
