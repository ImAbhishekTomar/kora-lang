//! What a package is allowed to do.
//!
//! A dependency has no ambient authority. It reaches the network, the
//! filesystem, a database, the environment, a Python worker, or an MCP server
//! only where the program that imported it said so, and it can never pass on
//! more than it holds itself. That is the property no existing package
//! ecosystem can retrofit: their packages run with the same rights as the
//! program, so a compromise anywhere in the graph is a compromise everywhere.
//!
//! Capabilities are coarse today — `net = true`, not a list of hosts. The
//! shape is chosen so a narrower form is a later superset rather than a
//! different field.

use std::collections::BTreeSet;

/// One coarse capability a package may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Net,
    Fs,
    Sql,
    Env,
    Python,
}

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Capability::Net => "net",
            Capability::Fs => "fs",
            Capability::Sql => "sql",
            Capability::Env => "env",
            Capability::Python => "python",
        }
    }

    /// The capability a stdlib module needs, if any.
    ///
    /// `json`, `csv`, `re`, and `time` compute over values the caller already
    /// has; there is nothing to gate. The rest reach outside the program.
    pub fn for_module(module: &str) -> Option<Capability> {
        match module {
            "http" => Some(Capability::Net),
            "fs" => Some(Capability::Fs),
            // The notes store is filesystem-backed (`.kora/notes/<run-id>.json`),
            // so a dependency needs the same grant `fs` does to touch it.
            "notes" => Some(Capability::Fs),
            "sql" => Some(Capability::Sql),
            "env" => Some(Capability::Env),
            _ => None,
        }
    }

    pub fn parse(name: &str) -> Option<Capability> {
        [
            Capability::Net,
            Capability::Fs,
            Capability::Sql,
            Capability::Env,
            Capability::Python,
        ]
        .into_iter()
        .find(|c| c.name() == name)
    }
}

/// The authority one package holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    capabilities: BTreeSet<Capability>,
    /// Sinks a `declassify` inside this package may name.
    sinks: BTreeSet<String>,
    /// MCP servers this package may connect to.
    mcp: BTreeSet<String>,
    /// Whether this package may `declassify` at all. Off by default: adding a
    /// dependency must not become the way to launder classified data.
    declassify: bool,
    /// The root program is bounded by its own kora.toml and nothing else.
    /// Without this every program written before grants existed would break.
    unrestricted: bool,
}

impl Grants {
    /// The root program's authority: everything.
    pub fn unrestricted() -> Grants {
        Grants {
            unrestricted: true,
            declassify: true,
            ..Grants::default()
        }
    }

    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted
    }

    pub fn allows(&self, capability: Capability) -> bool {
        self.unrestricted || self.capabilities.contains(&capability)
    }

    pub fn allows_sink(&self, sink: &str) -> bool {
        self.unrestricted || self.sinks.contains(sink)
    }

    pub fn allows_mcp(&self, server: &str) -> bool {
        self.unrestricted || self.mcp.contains(server)
    }

    pub fn allows_declassify(&self) -> bool {
        self.declassify
    }

    /// What this package holds that `other` does not.
    ///
    /// Used both to check a package's `[package.requires]` against what it
    /// was actually granted, and to report the difference in one message.
    pub fn missing_from(&self, other: &Grants) -> Vec<String> {
        if other.unrestricted {
            return Vec::new();
        }
        let mut out = Vec::new();
        for capability in &self.capabilities {
            if !other.capabilities.contains(capability) {
                out.push(capability.name().to_string());
            }
        }
        for sink in &self.sinks {
            if !other.sinks.contains(sink) {
                out.push(format!("sink `{sink}`"));
            }
        }
        for server in &self.mcp {
            if !other.mcp.contains(server) {
                out.push(format!("mcp `{server}`"));
            }
        }
        if self.declassify && !other.declassify {
            out.push("declassify".to_string());
        }
        out
    }

    /// The authority a parent may pass down: never more than it holds.
    ///
    /// A package granted nothing cannot grant its own dependencies anything,
    /// so compromising a leaf of the graph gains an attacker only what every
    /// link above it already had.
    pub fn capped_by(&self, parent: &Grants) -> Grants {
        if parent.unrestricted {
            return self.clone();
        }
        Grants {
            capabilities: self
                .capabilities
                .intersection(&parent.capabilities)
                .copied()
                .collect(),
            sinks: self.sinks.intersection(&parent.sinks).cloned().collect(),
            mcp: self.mcp.intersection(&parent.mcp).cloned().collect(),
            declassify: self.declassify && parent.declassify,
            unrestricted: false,
        }
    }

    /// Render for an error message or `kora tree`.
    pub fn describe(&self) -> String {
        if self.unrestricted {
            return "unrestricted".to_string();
        }
        let mut parts: Vec<String> = self
            .capabilities
            .iter()
            .map(|c| c.name().to_string())
            .collect();
        parts.extend(self.sinks.iter().map(|s| format!("sink:{s}")));
        parts.extend(self.mcp.iter().map(|s| format!("mcp:{s}")));
        if self.declassify {
            parts.push("declassify".to_string());
        }
        if parts.is_empty() {
            "nothing".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// Parse a `[package.requires]` or `[dependencies.x.grants]` table.
    pub fn from_toml(table: &toml::value::Table) -> Grants {
        let mut grants = Grants::default();
        for (key, value) in table {
            match key.as_str() {
                "sinks" => grants.sinks = string_set(value),
                "mcp" => grants.mcp = string_set(value),
                "declassify" => grants.declassify = value.as_bool().unwrap_or(false),
                other => {
                    if let Some(capability) = Capability::parse(other) {
                        if value.as_bool().unwrap_or(false) {
                            grants.capabilities.insert(capability);
                        }
                    }
                }
            }
        }
        grants
    }
}

fn string_set(value: &toml::Value) -> BTreeSet<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(toml_text: &str) -> Grants {
        let value: toml::Value = toml_text.parse().unwrap();
        Grants::from_toml(value.as_table().unwrap())
    }

    #[test]
    fn a_package_holds_only_what_is_listed() {
        let g = grants("net = true\nfs = false\nsinks = [\"stripe\"]\n");
        assert!(g.allows(Capability::Net));
        assert!(!g.allows(Capability::Fs));
        assert!(!g.allows(Capability::Sql));
        assert!(g.allows_sink("stripe"));
        assert!(!g.allows_sink("openai"));
    }

    #[test]
    fn declassify_is_off_unless_asked_for() {
        assert!(!grants("net = true\n").allows_declassify());
        assert!(grants("declassify = true\n").allows_declassify());
    }

    #[test]
    fn the_root_program_is_unrestricted() {
        let root = Grants::unrestricted();
        assert!(root.allows(Capability::Net));
        assert!(root.allows_sink("anything"));
        assert!(root.allows_mcp("anything"));
        assert!(root.allows_declassify());
    }

    #[test]
    fn a_parent_cannot_pass_on_what_it_does_not_hold() {
        let parent = grants("net = true\nsinks = [\"stripe\"]\n");
        let wanted = grants("net = true\nfs = true\nsinks = [\"stripe\", \"openai\"]\n");
        let effective = wanted.capped_by(&parent);
        assert!(effective.allows(Capability::Net));
        assert!(
            !effective.allows(Capability::Fs),
            "fs was never the parent's to give"
        );
        assert!(effective.allows_sink("stripe"));
        assert!(!effective.allows_sink("openai"));
    }

    #[test]
    fn the_root_program_caps_nothing() {
        let wanted = grants("net = true\nfs = true\n");
        let effective = wanted.capped_by(&Grants::unrestricted());
        assert!(effective.allows(Capability::Net));
        assert!(effective.allows(Capability::Fs));
    }

    #[test]
    fn missing_names_every_shortfall() {
        let required = grants("net = true\nsinks = [\"stripe\"]\ndeclassify = true\n");
        let granted = grants("net = true\n");
        let missing = required.missing_from(&granted);
        assert!(
            missing.contains(&"sink `stripe`".to_string()),
            "{missing:?}"
        );
        assert!(missing.contains(&"declassify".to_string()), "{missing:?}");
        assert!(!missing.contains(&"net".to_string()), "{missing:?}");
    }

    #[test]
    fn modules_map_to_the_capability_they_need() {
        assert_eq!(Capability::for_module("http"), Some(Capability::Net));
        assert_eq!(Capability::for_module("fs"), Some(Capability::Fs));
        assert_eq!(Capability::for_module("env"), Some(Capability::Env));
        // Pure computation over values the caller already holds.
        assert_eq!(Capability::for_module("json"), None);
        assert_eq!(Capability::for_module("re"), None);
    }
}
