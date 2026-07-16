use std::env;

pub fn guidance_enabled() -> bool {
    match env::var("FLUENT_QUIET") {
        Ok(val) => !matches!(val.as_str(), "1" | "true" | "yes"),
        Err(_) => true,
    }
}

pub fn after_work_item_create() -> &'static str {
    "\n→ Next: fluent attempt create <work-item-id>"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_work_item_create_names_attempt_create() {
        let hint = after_work_item_create();
        assert!(hint.contains("attempt create"));
    }
}
