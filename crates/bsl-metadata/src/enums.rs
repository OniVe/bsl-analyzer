use serde::{Deserialize, Serialize};
use stdx::case::CaseExt;

/// The declared installation scope of a configuration extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExtensionScope {
    /// The extension applies to the whole infobase.
    InfoBase,
    /// The extension applies to an individual data area.
    DataSeparation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ReturnValueReuse {
    #[serde(rename = "DontUse")]
    DontUse,
    #[serde(rename = "DuringRequest")]
    DuringRequest,
    #[serde(rename = "DuringSession")]
    DuringSession,
    #[serde(other)]
    #[default]
    Unknown,
}

impl ReturnValueReuse {
    pub fn from_name(name: &str) -> Self {
        let name_lower = name.fold_lower();
        match name_lower.as_str() {
            "dontuse" | "неиспользовать" => Self::DontUse,
            "duringrequest" | "навремязапроса" => Self::DuringRequest,
            "duringsession" | "навремясеанса" => Self::DuringSession,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ModuleType {
    #[serde(rename = "ApplicationModule")]
    ApplicationModule,
    #[serde(rename = "CommonModule")]
    CommonModule,
    #[serde(rename = "SessionModule")]
    SessionModule,
    #[serde(rename = "ExternalConnectionModule")]
    ExternalConnectionModule,
    #[serde(rename = "ManagedApplicationModule")]
    ManagedApplicationModule,
    #[serde(rename = "OrdinaryApplicationModule")]
    OrdinaryApplicationModule,
    #[serde(rename = "ManagerModule")]
    ManagerModule,
    #[serde(rename = "ObjectModule")]
    ObjectModule,
    #[serde(rename = "RecordSetModule")]
    RecordSetModule,
    #[serde(rename = "ValueManagerModule")]
    ValueManagerModule,
    #[serde(rename = "FormModule")]
    FormModule,
    #[serde(rename = "CommandModule")]
    CommandModule,
    #[serde(rename = "HTTPServiceModule")]
    HTTPServiceModule,
    #[serde(rename = "WEBServiceModule")]
    WebServiceModule,
    #[serde(rename = "IntegrationServiceModule")]
    IntegrationServiceModule,
    #[serde(other)]
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ObjectBelonging {
    #[serde(rename = "Own")]
    #[default]
    Own,
    #[serde(rename = "Adopted")]
    Adopted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum FormType {
    #[serde(rename = "Managed")]
    #[default]
    Managed,
    #[serde(rename = "Ordinary")]
    Ordinary,
    #[serde(other)]
    Unknown,
}

impl FormType {
    pub fn from_name(name: &str) -> Self {
        let name_lower = name.fold_lower();
        match name_lower.as_str() {
            "managed" | "управляемая" => Self::Managed,
            "ordinary" | "обычная" => Self::Ordinary,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum CodeSeries {
    #[serde(
        rename = "WholeCatalog",
        alias = "WholeCharacteristicKind",
        alias = "WholeChartOfAccounts"
    )]
    #[default]
    WholeCatalog,

    #[serde(rename = "WithinSubordination")]
    WithinSubordination,

    #[serde(rename = "WithinOwnerSubordination", alias = "WithinOwner")]
    WithinOwnerSubordination,

    #[serde(other)]
    Unknown,
}

impl CodeSeries {
    pub fn is_whole(&self) -> bool {
        matches!(self, Self::WholeCatalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_return_value_reuse_from_name() {
        assert_eq!(ReturnValueReuse::from_name("DontUse"), ReturnValueReuse::DontUse);
        assert_eq!(ReturnValueReuse::from_name("неиспользовать"), ReturnValueReuse::DontUse);
        assert_eq!(ReturnValueReuse::from_name("DuringRequest"), ReturnValueReuse::DuringRequest);
        assert_eq!(ReturnValueReuse::from_name("навремязапроса"), ReturnValueReuse::DuringRequest);
        assert_eq!(ReturnValueReuse::from_name("unknown"), ReturnValueReuse::Unknown);
    }

    #[test]
    fn test_default_values() {
        assert_eq!(ReturnValueReuse::default(), ReturnValueReuse::Unknown);
        assert_eq!(ModuleType::default(), ModuleType::Unknown);
        assert_eq!(ObjectBelonging::default(), ObjectBelonging::Own);
        assert_eq!(FormType::default(), FormType::Managed);
    }

    #[test]
    fn test_form_type_from_name() {
        assert_eq!(FormType::from_name("Managed"), FormType::Managed);
        assert_eq!(FormType::from_name("управляемая"), FormType::Managed);
        assert_eq!(FormType::from_name("Ordinary"), FormType::Ordinary);
        assert_eq!(FormType::from_name("обычная"), FormType::Ordinary);
        assert_eq!(FormType::from_name("unknown"), FormType::Unknown);
    }
}
