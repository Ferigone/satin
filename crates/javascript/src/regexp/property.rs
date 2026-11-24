//! Strict Unicode property and property-value lookup for ECMAScript RegExp.
use icu_properties::{
    CodePointMapData, CodePointSetData, CodePointSetDataBorrowed, PropertyParser,
    props::{GeneralCategory, GeneralCategoryGroup, Script},
    script::ScriptWithExtensions,
};

#[derive(Clone, Copy, Debug)]
pub(super) enum Property {
    Any,
    Ascii,
    Assigned,
    Binary(CodePointSetDataBorrowed<'static>),
    Category(GeneralCategoryGroup),
    Script(Script),
    Extensions(Script),
}

impl Property {
    pub(super) fn parse(name: &str) -> Option<Self> {
        let category = |v| {
            PropertyParser::<GeneralCategoryGroup>::new()
                .get_strict(v)
                .map(Self::Category)
        };
        let script = |v| PropertyParser::<Script>::new().get_strict(v);
        if let Some((key, value)) = name.split_once('=') {
            return match key {
                "gc" | "General_Category" => category(value),
                "sc" | "Script" => script(value).map(Self::Script),
                "scx" | "Script_Extensions" => script(value).map(Self::Extensions),
                _ => None,
            };
        }
        match name {
            "Any" => Some(Self::Any),
            "ASCII" => Some(Self::Ascii),
            "Assigned" => Some(Self::Assigned),
            _ => category(name)
                .or_else(|| CodePointSetData::new_for_ecma262(name.as_bytes()).map(Self::Binary)),
        }
    }

    pub(super) fn contains(self, point: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Ascii => point < 128,
            Self::Assigned => {
                CodePointMapData::<GeneralCategory>::new().get32(point)
                    != GeneralCategory::Unassigned
            }
            Self::Binary(set) => set.contains32(point),
            Self::Category(group) => {
                group.contains(CodePointMapData::<GeneralCategory>::new().get32(point))
            }
            Self::Script(script) => CodePointMapData::<Script>::new().get32(point) == script,
            Self::Extensions(script) => ScriptWithExtensions::new().has_script32(point, script),
        }
    }
}
