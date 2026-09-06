//! The runtime's module table and the editor's must agree.
//!
//! They live in different crates — `kora_runtime::stdlib::module` serves a
//! program, and `kora_types::MODULES` feeds completion and the "no such
//! module" hint — so a new stdlib module lands in the language and is
//! invisible in the editor until someone remembers the second list. That is
//! not hypothetical: it is exactly the kind of drift the whole
//! walk-the-whole-list rule in AGENTS.md exists to catch, and a test is
//! cheaper than the rule.

#[test]
fn the_editor_table_lists_every_module_the_runtime_serves() {
    for name in kora_runtime::stdlib::MODULE_NAMES {
        assert!(
            kora_types::module_functions(name).is_some(),
            "`{name}` is a runtime module with no entry in kora_types::MODULES, \
             so the editor cannot complete it"
        );
    }
}

#[test]
fn the_runtime_serves_every_module_the_editor_offers() {
    for name in kora_types::module_names() {
        assert!(
            kora_runtime::stdlib::module(name).is_some(),
            "the editor offers `{name}`, which the runtime does not serve"
        );
    }
}

#[test]
fn every_completed_function_is_one_the_runtime_exports() {
    // The failure this catches is worse than a missing name: an editor that
    // completes `yaml.loads` teaches a spelling the language does not have.
    for name in kora_types::module_names() {
        let module = kora_runtime::stdlib::module(name)
            .unwrap_or_else(|| panic!("`{name}` is offered but not served"));
        for function in kora_types::module_functions(name).unwrap_or(&[]) {
            assert!(
                module.functions.contains_key(function),
                "completion offers `{name}.{function}`, which the runtime does not export"
            );
        }
    }
}

#[test]
fn every_exported_function_is_one_the_editor_completes() {
    for name in kora_runtime::stdlib::MODULE_NAMES {
        let module = kora_runtime::stdlib::module(name).expect("a named module exists");
        let completed = kora_types::module_functions(name).unwrap_or(&[]);
        for function in module.functions.keys() {
            assert!(
                completed.contains(function),
                "the runtime exports `{name}.{function}`, which the editor never suggests"
            );
        }
    }
}
