pub fn version_tag() -> String {
    format!(
        "{} {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("FLUENT_BUILD_COMMIT").unwrap_or("unknown")
    )
}

pub fn version_string() -> String {
    format!("fluent {}", version_tag())
}

pub fn version_tag_str() -> &'static str {
    use std::sync::OnceLock;
    static TAG: OnceLock<String> = OnceLock::new();
    TAG.get_or_init(version_tag)
}
