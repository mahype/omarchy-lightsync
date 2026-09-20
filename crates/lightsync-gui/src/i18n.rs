use std::env;

#[cfg(test)]
use std::collections::BTreeSet;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use lightsync_domain::Language;
use unic_langid::LanguageIdentifier;

const EN: &str = include_str!("../resources/en.ftl");
const DE: &str = include_str!("../resources/de.ftl");

pub struct I18n {
    bundle: FluentBundle<FluentResource>,
}

impl I18n {
    pub fn new(language: Language) -> Self {
        let german = match language {
            Language::System => system_language_is_german(),
            Language::En => false,
            Language::De => true,
        };
        let locale: LanguageIdentifier = if german { "de-DE" } else { "en-US" }
            .parse()
            .expect("built-in locale is valid");
        let source = if german { DE } else { EN };
        let resource = FluentResource::try_new(source.to_owned())
            .unwrap_or_else(|(_, errors)| panic!("invalid embedded Fluent resource: {errors:?}"));
        let mut bundle = FluentBundle::new(vec![locale]);
        bundle
            .add_resource(resource)
            .expect("embedded messages have unique identifiers");
        Self { bundle }
    }

    pub fn text(&self, id: &str) -> String {
        self.format(id, None)
    }

    pub fn text_with_arg(&self, id: &str, name: &str, value: &str) -> String {
        let mut args = FluentArgs::new();
        args.set(name, value);
        self.format(id, Some(&args))
    }

    fn format(&self, id: &str, args: Option<&FluentArgs<'_>>) -> String {
        let message = self
            .bundle
            .get_message(id)
            .unwrap_or_else(|| panic!("missing translation: {id}"));
        let pattern = message
            .value()
            .unwrap_or_else(|| panic!("translation has no value: {id}"));
        let mut errors = Vec::new();
        self.bundle
            .format_pattern(pattern, args, &mut errors)
            .into_owned()
    }
}

fn system_language_is_german() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| env::var(name).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("de"))
}

#[cfg(test)]
fn message_ids(source: &str) -> BTreeSet<&str> {
    source
        .lines()
        .filter(|line| !line.starts_with([' ', '\t', '#']) && line.contains('='))
        .filter_map(|line| line.split_once('=').map(|(id, _)| id.trim()))
        .filter(|id| !id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_have_identical_message_ids() {
        assert_eq!(message_ids(EN), message_ids(DE));
    }

    #[test]
    fn catalogs_parse_and_all_messages_format() {
        for language in [Language::En, Language::De] {
            let i18n = I18n::new(language);
            for id in message_ids(EN) {
                assert!(!i18n.text(id).trim().is_empty(), "empty message: {id}");
            }
        }
    }

    #[test]
    fn contextual_action_labels_format_in_both_languages() {
        for language in [Language::En, Language::De] {
            let i18n = I18n::new(language);
            assert!(
                i18n.text_with_arg("action-delete-context", "name", "Movie")
                    .contains("Movie")
            );
            assert!(
                i18n.text_with_arg("delete-profile-body", "name", "Movie")
                    .contains("Movie")
            );
        }
    }
}
