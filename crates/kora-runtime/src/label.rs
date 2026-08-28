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

/// How secret a value is: may it be *sent out*?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Secrecy {
    #[default]
    Public,
    Classified,
}

/// How trustworthy a value is: may it be *acted on*?
///
/// Anything that entered the program from outside is `Unverified`: HTTP
/// bodies, file contents, model output, tool results. It becomes `Trusted`
/// only by being narrowed — parsed into a type, or matched against a finite
/// set. Never by assertion, which is the mistake that made Perl's taint mode
/// useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trust {
    #[default]
    Trusted,
    Unverified,
}

/// The two independent axes a value carries.
///
/// Confidentiality runs outward (secrets must not leave), integrity runs
/// inward (untrusted data must not reach a dangerous sink). Same machinery,
/// opposite directions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Label {
    pub secrecy: Secrecy,
    pub trust: Trust,
    /// The sink this value has been released to, if any.
    ///
    /// A `declassify ... for X:` block does **not** make the value plain: it
    /// records that X may receive it. Otherwise the block would release the
    /// value to everything inside it, so a secret declassified for a model
    /// could be written to a file three lines later.
    pub released: Option<Box<str>>,
}

impl Label {
    pub const PUBLIC: Label = Label {
        secrecy: Secrecy::Public,
        trust: Trust::Trusted,
        released: None,
    };

    pub const CLASSIFIED: Label = Label {
        secrecy: Secrecy::Classified,
        trust: Trust::Trusted,
        released: None,
    };

    /// Data that came from outside the program.
    pub const UNVERIFIED: Label = Label {
        secrecy: Secrecy::Public,
        trust: Trust::Unverified,
        released: None,
    };

    /// Combining two values yields the stricter label on both axes. This one
    /// rule is what makes propagation transitive across every operator.
    pub fn join(self, other: Label) -> Label {
        Label {
            secrecy: match (self.secrecy, other.secrecy) {
                (Secrecy::Public, Secrecy::Public) => Secrecy::Public,
                _ => Secrecy::Classified,
            },
            trust: match (self.trust, other.trust) {
                (Trust::Trusted, Trust::Trusted) => Trust::Trusted,
                _ => Trust::Unverified,
            },
            // Combining a released value with anything else drops the
            // release: the result is not the thing that was approved.
            released: match (&self.released, &other.released) {
                (Some(a), Some(b)) if a == b => Some(a.clone()),
                (Some(a), None) if other.secrecy == Secrecy::Public => Some(a.clone()),
                (None, Some(b)) if self.secrecy == Secrecy::Public => Some(b.clone()),
                _ => None,
            },
        }
    }

    /// Mark this value as approved for one named sink.
    pub fn released_to(mut self, sink: &str) -> Label {
        self.released = Some(sink.into());
        self
    }

    /// Whether this value may be given to `sink`.
    ///
    /// Public data may go anywhere. Classified data may go only where it was
    /// explicitly released.
    pub fn may_reach(&self, sink: &str) -> bool {
        if !self.is_classified() {
            return true;
        }
        self.released.as_deref() == Some(sink)
    }

    pub fn is_classified(&self) -> bool {
        self.secrecy == Secrecy::Classified
    }

    pub fn is_unverified(&self) -> bool {
        self.trust == Trust::Unverified
    }

    /// Whether the value carries any restriction at all.
    pub fn is_plain(&self) -> bool {
        self.secrecy == Secrecy::Public && self.trust == Trust::Trusted
    }

    /// Mark as having come from outside, keeping any secrecy already present.
    pub fn untrusted(mut self) -> Label {
        self.trust = Trust::Unverified;
        self
    }

    /// Narrowing succeeded: the value is now safe to act on.
    pub fn verified(mut self) -> Label {
        self.trust = Trust::Trusted;
        self
    }

    pub fn name(&self) -> &'static str {
        match (self.secrecy, self.trust) {
            (Secrecy::Classified, Trust::Unverified) => "classified and unverified",
            (Secrecy::Classified, Trust::Trusted) => "classified",
            (Secrecy::Public, Trust::Unverified) => "unverified",
            (Secrecy::Public, Trust::Trusted) => "public",
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
        if !label.is_classified() {
            return true;
        }
        self.allowed
            .get(sink)
            .is_some_and(|labels| labels.contains("classified"))
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
    fn join_takes_the_stricter_label_on_both_axes() {
        assert_eq!(Label::PUBLIC.join(Label::PUBLIC), Label::PUBLIC);
        assert!(Label::PUBLIC.join(Label::CLASSIFIED).is_classified());
        assert!(Label::PUBLIC.join(Label::UNVERIFIED).is_unverified());

        // The axes are independent: joining a secret with untrusted data
        // yields something that is both.
        let both = Label::CLASSIFIED.join(Label::UNVERIFIED);
        assert!(both.is_classified() && both.is_unverified());
        assert_eq!(both.name(), "classified and unverified");
    }

    #[test]
    fn verification_clears_only_the_trust_axis() {
        let both = Label::CLASSIFIED.join(Label::UNVERIFIED);
        let checked = both.verified();
        assert!(
            !checked.is_unverified(),
            "narrowing makes it safe to act on"
        );
        assert!(
            checked.is_classified(),
            "but it is still a secret: verifying is not declassifying"
        );
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
        assert!(p.permits("local_model", Label::CLASSIFIED));
        assert!(!p.permits("openai", Label::CLASSIFIED));
        // Public data flows anywhere.
        assert!(p.permits("openai", Label::PUBLIC));
    }

    #[test]
    fn deny_overrides_allow() {
        let p = policy(
            r#"
[sinks]
openai = { allow = ["classified"], deny = ["classified"] }
"#,
        );
        assert!(!p.permits("openai", Label::CLASSIFIED));
    }

    #[test]
    fn unknown_sink_refuses_classified() {
        // A typo must not silently open a hole.
        let p = policy("[sinks]\nlocal_model = { allow = [\"classified\"] }\n");
        assert!(!p.permits("locl_model", Label::CLASSIFIED));
        assert!(!p.is_known_sink("locl_model"));
    }

    #[test]
    fn empty_policy_denies_all_classified_flow() {
        let p = policy("");
        assert!(!p.permits("anything", Label::CLASSIFIED));
        assert!(p.permits("anything", Label::PUBLIC));
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
