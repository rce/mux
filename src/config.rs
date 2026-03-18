use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub scripts: Vec<ScriptConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ScriptConfig {
    pub name: String,
    pub cmd: String,
}

pub fn load(path: &str) -> Result<Config, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    parse(&content)
}

pub fn parse(content: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(content).map_err(|e| format!("invalid config: {e}"))?;

    if config.scripts.is_empty() {
        return Err("at least one [[scripts]] entry is required".into());
    }

    let mut seen = HashSet::new();
    for script in &config.scripts {
        if !seen.insert(&script.name) {
            return Err(format!("duplicate script name: {}", script.name));
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_parses_valid_config() {
        let toml = r#"
[[scripts]]
name = "whiskers"
cmd = "echo meow"

[[scripts]]
name = "paws"
cmd = "echo purr"
"#;
        let config = parse(toml).unwrap();
        assert_eq!(config.scripts.len(), 2);
        assert_eq!(config.scripts[0].name, "whiskers");
        assert_eq!(config.scripts[0].cmd, "echo meow");
        assert_eq!(config.scripts[1].name, "paws");
        assert_eq!(config.scripts[1].cmd, "echo purr");
    }

    #[test]
    fn parse_requires_at_least_one_script() {
        let toml = "scripts = []\n";
        let err = parse(toml).unwrap_err();
        assert!(err.contains("at least one"), "got: {err}");
    }

    #[test]
    fn parse_rejects_duplicate_names() {
        let toml = r#"
[[scripts]]
name = "nyan"
cmd = "echo 1"

[[scripts]]
name = "nyan"
cmd = "echo 2"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn parse_fails_on_invalid_toml() {
        let err = parse("this is not toml {{{").unwrap_err();
        assert!(err.contains("invalid config"), "got: {err}");
    }

    #[test]
    fn parse_fails_on_missing_fields() {
        let toml = r#"
[[scripts]]
name = "kitten"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.contains("invalid config"), "got: {err}");
    }
}
