use super::*;

#[test]
fn test_normalize_text() {
    assert_eq!(normalize_text("Hello, World!"), "hello world");
    assert_eq!(
        normalize_text("What is the cost of..."),
        "what is the cost of"
    );
    assert_eq!(normalize_text("  spaces   here  "), "spaces here");
}
