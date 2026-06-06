pub fn version_string() -> String {
    format!(
        "factory {} {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("FACTORY_BUILD_COMMIT").unwrap_or("unknown")
    )
}
