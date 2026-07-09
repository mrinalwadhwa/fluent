pub fn version_string() -> String {
    format!(
        "fluent {} {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("FLUENT_BUILD_COMMIT").unwrap_or("unknown")
    )
}
