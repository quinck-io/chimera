use super::*;

#[test]
fn masks_a_registered_secret() {
    let masker = SecretMasker::new(["hunter2"]);

    let masked = masker.mask("password is hunter2!");

    assert_eq!(masked, "password is ***!");
}

#[test]
fn masks_the_longer_of_two_overlapping_secrets_whole() {
    let masker = SecretMasker::new(["abc123", "abc123XYZ"]);

    let masked = masker.mask("token=abc123XYZ");

    assert_eq!(masked, "token=***");
}

#[test]
fn masks_each_line_of_a_multiline_secret() {
    let key = "-----BEGIN KEY-----\nMIIEvQIBADANBgkq\nhkiG9w0BAQEFAASC\n-----END KEY-----";
    let masker = SecretMasker::new([key]);

    let masked = masker.mask("  hkiG9w0BAQEFAASC");

    assert_eq!(masked, "  ***");
}

#[test]
fn ignores_blank_secrets() {
    let masker = SecretMasker::new(["", "  \n "]);

    let masked = masker.mask("nothing to hide");

    assert_eq!(masked, "nothing to hide");
}

#[test]
fn values_added_through_a_clone_are_shared() {
    let masker = SecretMasker::default();
    let clone = masker.clone();

    clone.add("late-secret");

    assert_eq!(masker.mask("late-secret"), "***");
}

#[test]
fn reveals_secret_only_when_text_contains_one() {
    let masker = SecretMasker::new(["s3cr3t"]);

    assert!(masker.reveals_secret("prefix-s3cr3t"));
    assert!(!masker.reveals_secret("harmless"));
}
