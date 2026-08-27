//! Information flow labels.
//!
//! A `classified` value must not reach a model, a tool, or any other sink
//! unless a `declassify ... for <sink>:` block explicitly allows it, and the
//! project's sink policy permits that label to travel there.
//!
//! The label is **transitive**: it survives slicing, formatting, arithmetic,
//! insertion into containers, and being returned from a function. Without
//! that, one line of laundering would defeat the whole feature — which is the
//! failure mode that killed Perl's taint mode and Ruby's `$SAFE`.

use std::collections::{HashMap, HashSet};

/// Confidentiality label on a value.
///
/// Binary for now (DECISIONS.md): `Public` or `Classified`. A small lattice
/// can be layered on later without changing call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Label {
    #[default]
    Public,
    Classified,
}

impl Label {
    /// Combining two values yields the stricter label. This single rule is
    /// what makes propagation transitive across every operator.
    pub fn join(self, other: Label) -> Label {
        match (self, other) {
            (Label::Public, Label::Public) => Label::Public,
            _ => Label::Classified,
        }
    }

    pub fn is_classified(self) -> bool {
        matches!(self, Label::Classified)
    }

    pub fn name(self) -> &'static str {
        match self {
            Label::Public => "public",
            Label::Classified => "classified",
        }
    }
}

/// Which labels a named sink accepts, from `[sinks]` in kora.toml.
#[derive(Debug, Clone, Default)]
pub struct SinkPolicy {
    /// sink name -> labels it may receive.
    allowed: HashMap<String, HashSet<String>>,
}

impl SinkPolicy {
    /// Parse the `[sinks]` table:
    ///
    /// ```toml
    /// [sinks]
    /// local_model = { allow = ["classified", "internal"] }
    /// openai      = { allow = ["internal"], deny = ["classified"] }
    /// ```
    pub fn from_toml(root: &toml::Value) -> SinkPolicy {
        let mut allowed = HashMap::new();
        if let Some(table) = root.get("sinks").and_then(|v| v.as_table()) {
            for (sink, spec) in table {
                let mut labels = HashSet::new();
                if let Some(list) = spec.get("allow").and_then(|v| v.as_array()) {
                    for entry in list {
                        if let Some(name) = entry.as_str() {
                            labels.insert(name.to_string());
                        }
                    }
                }
                // `deny` wins over `allow` for the same label.
                if let Some(list) = spec.get("deny").and_then(|v| v.as_array()) {
                    for entry in list {
                        if let Some(name) = entry.as_str() {
                            labels.remove(name);
                        }
                    }
                }
                allowed.insert(sink.clone(), labels);
            }
        }
        SinkPolicy { allowed }
    }

    /// May `label` be declassified to `sink`?
    ///
    /// Unknown sinks are refused rather than allowed: a typo in a sink name
    /// must not silently open a hole.
    pub fn permits(&self, sink: &str, label: Label) -> bool {
        match label {
            Label::Public => true,
            Label::Classified => self
                .allowed
                .get(sink)
                .is_some_and(|labels| labels.contains("classified")),
        }
    }

    pub fn is_known_sink(&self, sink: &str) -> bool {
        self.allowed.contains_key(sink)
    }

    /// Sinks that accept classified data, for error messages.
    pub fn sinks_accepting_classified(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .allowed
            .iter()
            .filter(|(_, labels)| labels.contains("classified"))
            .map(|(name, _)| name.clone())
            .collect();
        out.sort();
        out
    }

    pub fn known_sinks(&self) -> Vec<String> {
        let mut out: Vec<String> = self.allowed.keys().cloned().collect();
        out.sort();
        out
    }
}

/// One place in the program where classified data was released.
///
/// The compiler knows every site, which is what makes `kora audit` a complete
/// inventory rather than a best-effort grep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclassifySite {
    pub file: String,
    pub line: u32,
    pub expression: String,
    pub sink: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str) -> SinkPolicy {
        SinkPolicy::from_toml(&text.parse::<toml::Value>().unwrap())
    }

    #[test]
    fn join_takes_the_stricter_label() {
        assert_eq!(Label::Public.join(Label::Public), Label::Public);
        assert_eq!(Label::Public.join(Label::Classified), Label::Classified);
        assert_eq!(Label::Classified.join(Label::Public), Label::Classified);
        assert_eq!(Label::Classified.join(Label::Classified), Label::Classified);
    }

    #[test]
    fn allow_list_governs_classified_flow() {
        let p = policy(
            r#"
[sinks]
local_model = { allow = ["classified", "internal"] }
openai = { allow = ["internal"] }
"#,
        );
        assert!(p.permits("local_model", Label::Classified));
        assert!(!p.permits("openai", Label::Classified));
        // Public data flows anywhere.
        assert!(p.permits("openai", Label::Public));
    }

    #[test]
    fn deny_overrides_allow() {
        let p = policy(
            r#"
[sinks]
openai = { allow = ["classified"], deny = ["classified"] }
"#,
        );
        assert!(!p.permits("openai", Label::Classified));
    }

    #[test]
    fn unknown_sink_refuses_classified() {
        // A typo must not silently open a hole.
        let p = policy("[sinks]\nlocal_model = { allow = [\"classified\"] }\n");
        assert!(!p.permits("locl_model", Label::Classified));
        assert!(!p.is_known_sink("locl_model"));
    }

    #[test]
    fn empty_policy_denies_all_classified_flow() {
        let p = policy("");
        assert!(!p.permits("anything", Label::Classified));
        assert!(p.permits("anything", Label::Public));
    }

    #[test]
    fn reports_sinks_for_error_messages() {
        let p = policy(
            r#"
[sinks]
local_model = { allow = ["classified"] }
onprem = { allow = ["classified"] }
openai = { allow = ["internal"] }
"#,
        );
        assert_eq!(
            p.sinks_accepting_classified(),
            vec!["local_model".to_string(), "onprem".to_string()]
        );
        assert_eq!(p.known_sinks().len(), 3);
    }
}
