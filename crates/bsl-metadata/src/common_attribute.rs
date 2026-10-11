//! Common attributes (`ОбщиеРеквизиты`): fields the platform adds to many objects at once.
//!
//! A common attribute is declared once under `CommonAttributes/<Name>.xml` and then appears as
//! an ordinary field of every object in its composition (`Состав`) — in the object model
//! (`Ссылка.Организация`, `Объект.ОбластьДанныхОсновныеДанные`) and in query tables. The
//! objects' own XML does not mention it, so a field model built from that XML alone is short by
//! exactly these fields.
//!
//! Membership follows the platform rule (ITS, «Общие реквизиты»): an object listed in `Content`
//! with `Use` or `DontUse` is in or out regardless of anything else; an object not listed, or
//! listed with `Auto`, follows the attribute's `AutoUse`. Only the kinds that have fields are
//! ever reported as members; everything else (constants, scheduled jobs, ...) may sit in
//! `Content` but contributes no field this model could expose.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use stdx::case::CaseExt;
use uuid::Uuid;

use crate::enums::{ExtensionScope, ObjectBelonging};
use crate::metadata_object::{AttributeType, MdoType};

/// One object's setting in a common attribute's `Content` (`Состав`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommonAttributeUse {
    Use,
    DontUse,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationExtensionsSeparation {
    Separate,
    DontUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeparatedDataUse {
    Independently,
    IndependentlyAndSimultaneously,
}

/// A parsed `CommonAttributes/<Name>.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonAttribute {
    pub name: String,
    pub uuid: Option<Uuid>,
    pub object_belonging: ObjectBelonging,
    pub extended_configuration_object: Option<Uuid>,
    pub attr_type: AttributeType,
    pub attr_type_present: bool,
    /// `AutoUse = Use`: objects without an explicit setting are members.
    pub auto_use: bool,
    /// `DataSeparation = Separate/DontUse`; malformed or omitted values remain unknown.
    pub data_separation: Option<bool>,
    pub data_separation_present: bool,
    pub configuration_extensions_separation: Option<ConfigurationExtensionsSeparation>,
    pub separated_data_use: Option<SeparatedDataUse>,
    pub separated_data_use_present: bool,
    /// Explicit settings keyed by `(kind, folded object name)`.
    pub content: FxHashMap<(MdoType, String), CommonAttributeUse>,
    /// A `Content` entry this reader could not attach to an object while it could still have
    /// put one INTO the composition. With `AutoUse = DontUse` such an entry is the only way an
    /// object becomes a member, so membership is then unknown for every object; with
    /// `AutoUse = Use` an unreadable entry can only remove the field, and over-reporting a
    /// field silences a diagnostic rather than inventing one.
    pub unattributed_members: bool,
}

impl CommonAttribute {
    pub fn estimated_heap_size(&self) -> usize {
        self.name.capacity()
            + self.attr_type.estimated_heap_size()
            + stdx::heap::map_table_bytes::<(MdoType, String), CommonAttributeUse>(
                self.content.len(),
            )
            + self.content.keys().map(|(_, name)| name.capacity()).sum::<usize>()
    }

    /// Raw membership under this declaration's own AutoUse/Content rules.
    pub fn includes(&self, mdo_type: MdoType, object_name: &str) -> Option<bool> {
        if !kind_takes_common_attributes(mdo_type) {
            return Some(false);
        }
        let explicit = self.content.get(&(mdo_type, object_name.fold_lower())).copied();
        match explicit {
            Some(CommonAttributeUse::Use) => Some(true),
            Some(CommonAttributeUse::DontUse) => Some(false),
            Some(CommonAttributeUse::Auto) | None if self.auto_use => Some(true),
            Some(CommonAttributeUse::Auto) | None if self.unattributed_members => None,
            Some(CommonAttributeUse::Auto) | None => Some(false),
        }
    }
}

/// A field a common attribute contributes to one object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommonAttributeField {
    pub name: String,
    pub attr_type: AttributeType,
    /// A data separator. In register tables it behaves like a dimension: it is part of the
    /// record key and a column of every virtual table.
    pub separator: bool,
}

impl CommonAttributeField {
    pub fn estimated_heap_size(&self) -> usize {
        self.name.capacity() + self.attr_type.estimated_heap_size()
    }
}

/// The common attributes that apply to one object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectCommonAttributes {
    pub fields: Vec<CommonAttributeField>,
    /// Membership could not be decided for at least one common attribute, so the object may
    /// carry a field this list does not name.
    pub open: bool,
}

impl ObjectCommonAttributes {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && !self.open
    }
}

/// Whether the platform lets a common attribute add a field to an object of this kind.
///
/// The set is the ITS list of kinds a common attribute may include, narrowed to the kinds this
/// model gives fields to. Constants, scheduled jobs and the like may sit in `Content` (a
/// separator divides their data too) but expose no field to code or queries.
pub fn kind_takes_common_attributes(mdo_type: MdoType) -> bool {
    matches!(
        mdo_type,
        MdoType::Catalog
            | MdoType::Document
            | MdoType::ChartOfCharacteristicTypes
            | MdoType::ChartOfAccounts
            | MdoType::ChartOfCalculationTypes
            | MdoType::BusinessProcess
            | MdoType::Task
            | MdoType::ExchangePlan
            | MdoType::InformationRegister
            | MdoType::AccumulationRegister
            | MdoType::AccountingRegister
            | MdoType::CalculationRegister
    )
}

/// What a `Content` entry's `xr:Metadata` reference names.
pub(crate) enum ContentTarget {
    /// A top-level object of a kind this model represents.
    Object(MdoType, String),
    /// A valid reference to something that never carries a field here: a nested object
    /// (`CalculationRegister.X.Recalculation.Y`) or a kind without fields in this model.
    Ignored,
    /// A reference this reader does not understand.
    Unknown,
}

/// Kinds that legitimately appear in `Content` but are not represented by [`MdoType`].
const KNOWN_UNMODELED_KINDS: &[&str] = &[
    "DocumentJournal",
    "Sequence",
    "ScheduledJob",
    "SessionParameter",
    "SettingsStorage",
    "FilterCriterion",
    "ExternalDataSource",
];

pub(crate) fn classify_content_reference(reference: &str) -> ContentTarget {
    let mut segments = reference.split('.');
    let (Some(kind), Some(name)) = (segments.next(), segments.next()) else {
        return ContentTarget::Unknown;
    };
    if kind.is_empty() || name.is_empty() {
        return ContentTarget::Unknown;
    }
    let nested = segments.next().is_some();
    if KNOWN_UNMODELED_KINDS.iter().any(|known| known.eq_ignore_ascii_case(kind)) {
        return ContentTarget::Ignored;
    }
    match kind.parse::<MdoType>() {
        Ok(_) if nested => ContentTarget::Ignored,
        Ok(mdo_type) => ContentTarget::Object(mdo_type, name.to_string()),
        Err(_) => ContentTarget::Unknown,
    }
}

/// All common attributes of one configuration root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommonAttributeSet {
    pub attributes: Vec<CommonAttribute>,
    /// A `CommonAttributes/*.xml` file exists but could not be read: some field of unknown
    /// name may sit on any object.
    pub unreadable: bool,
}

impl CommonAttributeSet {
    pub fn is_empty(&self) -> bool {
        self.attributes.is_empty() && !self.unreadable
    }

    /// Add the result of parsing one `CommonAttributes/*.xml`; `None` marks the set unreadable.
    pub fn push_parsed(&mut self, parsed: Option<CommonAttribute>) {
        match parsed {
            Some(attribute) => self.attributes.push(attribute),
            None => self.unreadable = true,
        }
    }

    pub fn estimated_heap_size(&self) -> usize {
        stdx::heap::vec_bytes::<CommonAttribute>(self.attributes.len())
            + self.attributes.iter().map(CommonAttribute::estimated_heap_size).sum::<usize>()
    }

    /// The common-attribute fields of one object, and whether that list may be short.
    pub fn for_object(&self, mdo_type: MdoType, object_name: &str) -> ObjectCommonAttributes {
        if !kind_takes_common_attributes(mdo_type) {
            return ObjectCommonAttributes::default();
        }
        let mut out = ObjectCommonAttributes { fields: Vec::new(), open: self.unreadable };
        for attribute in &self.attributes {
            match attribute.includes(mdo_type, object_name) {
                Some(true) => add_visible_attribute(&mut out, attribute),
                Some(false) => {}
                None => out.open = true,
            }
        }
        out
    }

    /// Resolve fields for an extension-owned object using its local declarations and a
    /// confirmed main-configuration set. The returned fields are the application-visible
    /// projection; raw membership remains represented by the source sets.
    pub fn for_extension_object(
        &self,
        base: Option<&CommonAttributeSet>,
        scope: ExtensionScope,
        mdo_type: MdoType,
        object_name: &str,
    ) -> ObjectCommonAttributes {
        if !kind_takes_common_attributes(mdo_type) {
            return ObjectCommonAttributes::default();
        }
        let mut out = ObjectCommonAttributes { fields: Vec::new(), open: self.unreadable };

        for local in &self.attributes {
            if local.object_belonging == ObjectBelonging::Own {
                match local.includes(mdo_type, object_name) {
                    Some(true) => add_visible_attribute(&mut out, local),
                    Some(false) => {}
                    None => out.open = true,
                }
            } else if local.object_belonging == ObjectBelonging::Unknown
                && (adopted_content_may_include(local, mdo_type, object_name) || local.auto_use)
            {
                out.open = true;
            }
        }

        let Some(base) = base else {
            if self.attributes.iter().any(|attribute| {
                attribute.object_belonging == ObjectBelonging::Adopted
                    && adopted_content_may_include(attribute, mdo_type, object_name)
            }) {
                out.open = true;
            }
            return out;
        };

        if base.unreadable {
            out.open = true;
        }

        if scope == ExtensionScope::DataSeparation {
            for main in &base.attributes {
                match automatic_area_membership(main, scope) {
                    Some(true) => add_visible_attribute(&mut out, main),
                    Some(false) => {}
                    None => out.open = true,
                }
            }
        }

        for local in &self.attributes {
            if local.object_belonging != ObjectBelonging::Adopted
                || !adopted_content_may_include(local, mdo_type, object_name)
            {
                continue;
            }
            let Some(extended_uuid) = local.extended_configuration_object else {
                out.open = true;
                continue;
            };
            let Some(main) =
                base.attributes.iter().find(|attribute| attribute.uuid == Some(extended_uuid))
            else {
                out.open = true;
                continue;
            };

            let automatic_area_member = automatic_area_membership(main, scope) == Some(true);
            let explicit_member = match local.content.get(&(mdo_type, object_name.fold_lower())) {
                Some(CommonAttributeUse::Use) => Some(true),
                Some(CommonAttributeUse::DontUse) => Some(false),
                Some(CommonAttributeUse::Auto) => None,
                None if local.unattributed_members => None,
                None => Some(false),
            };
            let included = if automatic_area_member { Some(true) } else { explicit_member };
            match included {
                Some(false) => {}
                None => out.open = true,
                Some(true) => {
                    add_inherited_attribute(&mut out, local, main);
                }
            }
        }
        out
    }
}

fn automatic_area_membership(attribute: &CommonAttribute, scope: ExtensionScope) -> Option<bool> {
    if scope != ExtensionScope::DataSeparation {
        return Some(false);
    }
    match attribute.data_separation {
        Some(true) => match attribute.configuration_extensions_separation {
            Some(ConfigurationExtensionsSeparation::Separate) => Some(true),
            Some(ConfigurationExtensionsSeparation::DontUse) => Some(false),
            None => None,
        },
        Some(false) => Some(false),
        None => None,
    }
}

fn adopted_content_may_include(
    attribute: &CommonAttribute,
    mdo_type: MdoType,
    object_name: &str,
) -> bool {
    matches!(
        attribute.content.get(&(mdo_type, object_name.fold_lower())),
        Some(CommonAttributeUse::Use | CommonAttributeUse::DontUse | CommonAttributeUse::Auto)
    ) || attribute.unattributed_members
}

fn add_visible_attribute(out: &mut ObjectCommonAttributes, attribute: &CommonAttribute) {
    let Some(separator) = attribute.data_separation else {
        out.open = true;
        return;
    };
    if separator {
        match attribute.separated_data_use {
            Some(SeparatedDataUse::Independently) => return,
            Some(SeparatedDataUse::IndependentlyAndSimultaneously) => {}
            None if !attribute.separated_data_use_present => return,
            None => {
                out.open = true;
                return;
            }
        }
    }
    if attribute.attr_type == AttributeType::Unknown {
        out.open = true;
        return;
    }
    merge_common_attribute_fields(
        &mut out.fields,
        &[CommonAttributeField {
            name: attribute.name.clone(),
            attr_type: attribute.attr_type.clone(),
            separator,
        }],
    );
}

fn add_inherited_attribute(
    out: &mut ObjectCommonAttributes,
    local: &CommonAttribute,
    main: &CommonAttribute,
) {
    let inherited = CommonAttribute {
        name: local.name.clone(),
        uuid: local.uuid,
        object_belonging: local.object_belonging,
        extended_configuration_object: local.extended_configuration_object,
        attr_type: if local.attr_type_present {
            local.attr_type.clone()
        } else {
            main.attr_type.clone()
        },
        attr_type_present: local.attr_type_present,
        auto_use: local.auto_use,
        data_separation: if local.data_separation_present {
            local.data_separation
        } else {
            main.data_separation
        },
        data_separation_present: local.data_separation_present,
        configuration_extensions_separation: local
            .configuration_extensions_separation
            .or(main.configuration_extensions_separation),
        separated_data_use: if local.separated_data_use_present {
            local.separated_data_use
        } else if main.separated_data_use_present {
            main.separated_data_use
        } else {
            Some(SeparatedDataUse::Independently)
        },
        separated_data_use_present: local.separated_data_use_present
            || main.separated_data_use_present,
        content: local.content.clone(),
        unattributed_members: local.unattributed_members,
    };
    add_visible_attribute(out, &inherited);
}

/// Fold `overlay` into `base` the way an extension overlay folds members: a same-named field is
/// replaced, a new one added, and openness accumulates.
pub(crate) fn merge_common_attribute_fields(
    base: &mut Vec<CommonAttributeField>,
    overlay: &[CommonAttributeField],
) {
    for field in overlay {
        base.retain(|existing| existing.name.fold_lower() != field.name.fold_lower());
        base.push(field.clone());
    }
    base.sort_by_key(|field| field.name.fold_lower());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(
        auto_use: bool,
        content: &[(MdoType, &str, CommonAttributeUse)],
    ) -> CommonAttribute {
        CommonAttribute {
            name: "Организация".to_string(),
            uuid: None,
            object_belonging: ObjectBelonging::Own,
            extended_configuration_object: None,
            attr_type: AttributeType::String { length: Some(10) },
            attr_type_present: true,
            auto_use,
            data_separation: Some(false),
            data_separation_present: true,
            configuration_extensions_separation: None,
            separated_data_use: Some(SeparatedDataUse::Independently),
            separated_data_use_present: true,
            content: content
                .iter()
                .map(|(kind, name, usage)| ((*kind, name.fold_lower()), *usage))
                .collect(),
            unattributed_members: false,
        }
    }

    #[test]
    fn explicit_setting_wins_over_auto_use() {
        let attr = attribute(
            true,
            &[
                (MdoType::Catalog, "Исключенный", CommonAttributeUse::DontUse),
                (MdoType::Catalog, "Явный", CommonAttributeUse::Use),
            ],
        );
        assert_eq!(attr.includes(MdoType::Catalog, "Исключенный"), Some(false));
        assert_eq!(attr.includes(MdoType::Catalog, "явный"), Some(true));
        assert_eq!(attr.includes(MdoType::Catalog, "НеУпомянутый"), Some(true));

        let attr = attribute(false, &[(MdoType::Document, "Явный", CommonAttributeUse::Use)]);
        assert_eq!(attr.includes(MdoType::Document, "Явный"), Some(true));
        assert_eq!(attr.includes(MdoType::Document, "НеУпомянутый"), Some(false));
    }

    #[test]
    fn auto_entry_follows_auto_use() {
        let on = attribute(true, &[(MdoType::Catalog, "А", CommonAttributeUse::Auto)]);
        let off = attribute(false, &[(MdoType::Catalog, "А", CommonAttributeUse::Auto)]);
        assert_eq!(on.includes(MdoType::Catalog, "А"), Some(true));
        assert_eq!(off.includes(MdoType::Catalog, "А"), Some(false));
    }

    #[test]
    fn kinds_without_fields_never_take_the_attribute() {
        let attr = attribute(true, &[(MdoType::Constant, "К", CommonAttributeUse::Use)]);
        assert_eq!(attr.includes(MdoType::Constant, "К"), Some(false));
        assert_eq!(attr.includes(MdoType::Enum, "П"), Some(false));
    }

    #[test]
    fn unattributed_entry_leaves_membership_open_only_without_auto_use() {
        let mut attr = attribute(false, &[]);
        attr.unattributed_members = true;
        assert_eq!(attr.includes(MdoType::Catalog, "А"), None);
        attr.auto_use = true;
        assert_eq!(attr.includes(MdoType::Catalog, "А"), Some(true));
    }

    #[test]
    fn content_references_are_classified() {
        assert!(matches!(
            classify_content_reference("Catalog.Товары"),
            ContentTarget::Object(MdoType::Catalog, name) if name == "Товары"
        ));
        assert!(matches!(classify_content_reference("ScheduledJob.Х"), ContentTarget::Ignored));
        assert!(matches!(
            classify_content_reference("CalculationRegister.Р.Recalculation.П"),
            ContentTarget::Ignored
        ));
        assert!(matches!(classify_content_reference("Nonsense.Х"), ContentTarget::Unknown));
        assert!(matches!(classify_content_reference("Catalog"), ContentTarget::Unknown));
    }

    fn names(fields: &[CommonAttributeField]) -> Vec<(&str, bool)> {
        let mut out: Vec<_> = fields.iter().map(|f| (f.name.as_str(), f.separator)).collect();
        out.sort();
        out
    }

    #[test]
    fn a_loaded_configuration_attaches_fields_by_composition() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/common_attributes");
        let config = crate::load_from_directory(root).expect("the fixture loads");

        let catalog = config.find_metadata_object(MdoType::Catalog, "Справочник1").unwrap();
        assert_eq!(
            names(&catalog.common_attributes),
            [
                ("ОбластьДанныхОсновныеДанные", true),
                ("ОбщийКомментарий", false),
                ("Организация", false)
            ]
        );
        assert!(!catalog.common_attributes_open);
        // Own attributes stay what the object declares.
        assert!(catalog.attributes.iter().all(|a| a.name != "Организация"));

        let excluded =
            config.find_metadata_object(MdoType::Catalog, "СправочникБезРазделения").unwrap();
        assert_eq!(names(&excluded.common_attributes), [("ОбщийКомментарий", false)]);

        let document = config.find_metadata_object(MdoType::Document, "Документ1").unwrap();
        assert_eq!(
            names(&document.common_attributes),
            [("ОбластьДанныхОсновныеДанные", true), ("Организация", false)]
        );

        let info = config
            .find_register_by_type_and_name(MdoType::InformationRegister, "РегистрСведений1")
            .unwrap();
        assert_eq!(
            names(info.common_attributes()),
            [
                ("ОбластьДанныхОсновныеДанные", true),
                ("ОбщийКомментарий", false),
                ("Организация", false)
            ]
        );
        let accumulation = config
            .find_register_by_type_and_name(MdoType::AccumulationRegister, "РегистрНакопления1")
            .unwrap();
        assert_eq!(
            names(accumulation.common_attributes()),
            [("ОбластьДанныхОсновныеДанные", true), ("ОбщийКомментарий", false)]
        );
    }

    #[test]
    fn unreadable_file_opens_every_object_that_takes_fields() {
        let set = CommonAttributeSet { attributes: Vec::new(), unreadable: true };
        assert!(set.for_object(MdoType::Catalog, "А").open);
        assert!(!set.for_object(MdoType::Enum, "П").open);
    }

    fn fixture_set(path: &str) -> CommonAttributeSet {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let directory = path.join("CommonAttributes");
        if !directory.is_dir() {
            return CommonAttributeSet::default();
        }
        let mut files = std::fs::read_dir(directory)
            .expect("fixture CommonAttributes directory")
            .map(|entry| entry.expect("fixture entry").path())
            .collect::<Vec<_>>();
        files.sort();
        let mut set = CommonAttributeSet::default();
        for file in files {
            let xml = std::fs::read_to_string(file).expect("fixture XML");
            set.push_parsed(crate::xml_parser::parse_common_attribute_xml(&xml).ok());
        }
        set
    }

    fn field_names(fields: &[CommonAttributeField]) -> Vec<&str> {
        let mut names = fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    #[test]
    fn separator_membership_and_simultaneous_visibility_are_both_preserved() {
        let set = fixture_set("fixtures/common_attribute_separator");
        let separator = set
            .attributes
            .iter()
            .find(|attribute| attribute.name == "ОбластьДанныхВспомогательныеДанные")
            .unwrap();
        assert_eq!(
            separator.includes(MdoType::InformationRegister, "РазделенныеНастройки"),
            Some(true)
        );
        assert_eq!(separator.data_separation, Some(true));
        assert_eq!(
            set.for_object(MdoType::InformationRegister, "РазделенныеНастройки")
                .fields
                .iter()
                .find(|field| field.name == separator.name)
                .map(|field| field.separator),
            Some(true)
        );
    }

    #[test]
    fn independently_separator_is_a_member_but_not_an_application_field() {
        let set = fixture_set("fixtures/extension_common_attributes/base");
        let separator =
            set.attributes.iter().find(|attribute| attribute.name == "AreaUse").unwrap();
        assert_eq!(separator.includes(MdoType::Catalog, "BaseCatalog"), Some(true));
        assert_eq!(separator.separated_data_use, Some(SeparatedDataUse::Independently));
        assert_eq!(
            automatic_area_membership(separator, ExtensionScope::DataSeparation),
            Some(true)
        );
        let projected = set.for_object(MdoType::Catalog, "BaseCatalog");
        assert!(!projected.fields.iter().any(|field| field.name == "AreaUse"));
        assert!(!projected.open);
    }

    #[test]
    fn scope_data_separation_adds_only_base_area_separators_regardless_of_auto_use() {
        let base = fixture_set("fixtures/extension_common_attributes/base");
        let area_use =
            base.attributes.iter().find(|attribute| attribute.name == "AreaUse").unwrap();
        let area_no_auto =
            base.attributes.iter().find(|attribute| attribute.name == "AreaNoAuto").unwrap();
        let other_use =
            base.attributes.iter().find(|attribute| attribute.name == "OtherUse").unwrap();
        assert!(area_use.auto_use);
        assert!(!area_no_auto.auto_use);
        assert_eq!(
            area_use.configuration_extensions_separation,
            Some(ConfigurationExtensionsSeparation::Separate)
        );
        assert_eq!(
            area_no_auto.configuration_extensions_separation,
            Some(ConfigurationExtensionsSeparation::Separate)
        );
        assert_eq!(
            other_use.configuration_extensions_separation,
            Some(ConfigurationExtensionsSeparation::DontUse)
        );
        assert_eq!(automatic_area_membership(area_use, ExtensionScope::DataSeparation), Some(true));
        assert_eq!(
            automatic_area_membership(area_no_auto, ExtensionScope::DataSeparation),
            Some(true)
        );
        assert_eq!(
            automatic_area_membership(other_use, ExtensionScope::DataSeparation),
            Some(false)
        );

        let no_common = fixture_set("fixtures/extension_common_attributes/extensions/no-common");
        let projected = no_common.for_extension_object(
            Some(&base),
            ExtensionScope::DataSeparation,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(projected.fields.is_empty());
        assert!(!projected.open);
    }

    #[test]
    fn extension_matrix_separates_area_membership_from_field_visibility() {
        let base = fixture_set("fixtures/extension_common_attributes/base");
        let no_common = fixture_set("fixtures/extension_common_attributes/extensions/no-common");
        let explicit_use =
            fixture_set("fixtures/extension_common_attributes/extensions/explicit-use");
        let explicit_dont_use =
            fixture_set("fixtures/extension_common_attributes/extensions/explicit-dont-use");

        let automatic = no_common.for_extension_object(
            Some(&base),
            ExtensionScope::DataSeparation,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(automatic.fields.is_empty());
        assert!(!automatic.open);

        let global = no_common.for_extension_object(
            Some(&base),
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(global.fields.is_empty());
        assert!(!global.open);

        let explicit = explicit_use.for_extension_object(
            Some(&base),
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert_eq!(field_names(&explicit.fields), ["OrdinaryAuto", "OtherUse"]);
        assert!(explicit
            .fields
            .iter()
            .all(|field| field.attr_type == (AttributeType::Number { precision: 7, scale: 0 })));
        assert!(!explicit.fields.iter().any(|field| field.name == "AreaUse"));
        assert!(!explicit.open);

        let area_explicit = explicit_use.for_extension_object(
            Some(&base),
            ExtensionScope::DataSeparation,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert_eq!(field_names(&area_explicit.fields), ["OrdinaryAuto", "OtherUse"]);
        assert!(!area_explicit.fields.iter().any(|field| field.name == "AreaUse"));
        assert!(!area_explicit.open);

        let excluded = explicit_dont_use.for_extension_object(
            Some(&base),
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(excluded.fields.is_empty());
        assert!(!excluded.open);
    }

    #[test]
    fn missing_base_and_same_name_uuid_mismatch_remain_open_without_a_fake_field() {
        let base = fixture_set("fixtures/extension_common_attributes/base");
        let explicit = fixture_set("fixtures/extension_common_attributes/extensions/explicit-use");
        let missing_base = explicit.for_extension_object(
            None,
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(missing_base.fields.is_empty());
        assert!(missing_base.open);

        let mut wrong_base = base.clone();
        let target = wrong_base
            .attributes
            .iter_mut()
            .find(|attribute| attribute.name == "OtherUse")
            .unwrap();
        target.uuid = Some(Uuid::new_v4());
        let mismatched = explicit.for_extension_object(
            Some(&wrong_base),
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert!(!mismatched.fields.iter().any(|field| field.name == "OtherUse"));
        assert!(mismatched.open);
    }

    #[test]
    fn constants_and_enums_never_acquire_extension_common_fields_or_open() {
        let base = fixture_set("fixtures/extension_common_attributes/base");
        let mut local = fixture_set("fixtures/extension_common_attributes/extensions/explicit-use");
        local.unreadable = true;
        for scope in [ExtensionScope::InfoBase, ExtensionScope::DataSeparation] {
            for kind in [MdoType::Constant, MdoType::Enum] {
                let applied = local.for_extension_object(Some(&base), scope, kind, "NativeCatalog");
                assert!(applied.fields.is_empty());
                assert!(!applied.open);
            }
        }
    }

    #[test]
    fn malformed_inherited_visibility_keeps_known_fields_and_does_not_assume_default() {
        let mut base = fixture_set("fixtures/extension_common_attributes/base");
        let area =
            base.attributes.iter_mut().find(|attribute| attribute.name == "AreaUse").unwrap();
        area.separated_data_use = None;
        area.separated_data_use_present = true;
        let explicit_use =
            fixture_set("fixtures/extension_common_attributes/extensions/explicit-use");
        let projected = explicit_use.for_extension_object(
            Some(&base),
            ExtensionScope::InfoBase,
            MdoType::Catalog,
            "NativeCatalog",
        );
        assert_eq!(field_names(&projected.fields), ["OrdinaryAuto", "OtherUse"]);
        assert!(projected.open);
    }
}
