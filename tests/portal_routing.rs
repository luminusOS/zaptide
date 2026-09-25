use zaptide::native_portals::{ValidationError, validate_uri};

#[test]
fn only_safe_uri_schemes_are_opened() {
    assert_eq!(
        validate_uri("https://example.org/page").unwrap().as_str(),
        "https://example.org/page"
    );
    assert!(validate_uri("mailto:user@example.org").is_ok());
    assert!(validate_uri("javascript:alert(1)").is_err());
    assert!(validate_uri("file:///etc/passwd").is_err());
    assert!(validate_uri("").is_err());
    assert_eq!(
        validate_uri("data:text/html,<script>").unwrap_err(),
        ValidationError::UnsupportedScheme
    );
}
