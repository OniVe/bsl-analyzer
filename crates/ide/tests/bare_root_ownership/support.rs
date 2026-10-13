//! Fixtures for "who owns the bare name `Справочники`".
//!
//! One scenario builder per claiming symbol, so every surface asks the same
//! question of the same set of owners. The designer fixture covers only the
//! owners a module body can declare on its own; an attribute, a form attribute
//! or a common module named after a manager collection does not exist there and
//! is built as a synthetic configuration on disk.

use ide::Analysis;
use ide_db::base_db::{SourceDatabase, SourceRoot, SourceRootId};
use ide_db::RootDatabaseImpl;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use test_fixture::Fixture;
use vfs::{FileId, FileSet};

/// The manager-collection name every scenario fights over.
pub(super) const ROOT: &str = "Справочники";

/// The user symbol claiming [`ROOT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    /// Nobody claims it — the platform collection owns the name.
    Unheld,
    /// Nobody claims it, in a hand-written configuration. Tells "shadowing
    /// works" apart from "this configuration resolves nothing at all".
    UnheldSynthetic,
    /// A bare `Справочники = …` with no declaration. This claims NOTHING: the
    /// name belongs to a Global-context property, and the platform refuses the
    /// write instead of declaring a local, so the collection keeps the name. It
    /// serves as a control — the surfaces must still answer for the platform.
    ImplicitAssignment,
    Parameter,
    ModuleVariable,
    ModuleMethod,
    ObjectAttribute,
    FormAttribute,
    CommonModule,
}

impl Owner {
    /// Every owner that claims the name. A surface must offer the platform
    /// answer for none of them. Enumerated, not sampled: fixing one
    /// representative and calling the class closed is what let the same defect
    /// return through a different owner four times.
    pub(super) const HELD: [Owner; 6] = [
        Owner::Parameter,
        Owner::ModuleVariable,
        Owner::ModuleMethod,
        Owner::ObjectAttribute,
        Owner::FormAttribute,
        Owner::CommonModule,
    ];

    /// Every case where the name still denotes the platform collection. These are
    /// the controls: without them an absent platform answer would equally mean
    /// "ownership is respected" and "this surface answers nothing here".
    pub(super) const UNHELD: [Owner; 3] =
        [Owner::Unheld, Owner::UnheldSynthetic, Owner::ImplicitAssignment];

    /// The module text claiming the name around `stmt`.
    fn wrap(self, stmt: &str) -> String {
        match self {
            Owner::ImplicitAssignment => format!(
                "Функция Тест()\n    Справочники = НеизвестнаяФункция();\n    \
                 {stmt}\nКонецФункции\n"
            ),
            Owner::Parameter => format!("Функция Тест(Справочники)\n    {stmt}\nКонецФункции\n"),
            Owner::ModuleVariable => {
                format!("Перем Справочники;\n\nФункция Тест()\n    {stmt}\nКонецФункции\n")
            }
            Owner::ModuleMethod => format!(
                "Функция Справочники()\n    Возврат Неопределено;\nКонецФункции\n\n\
                 Функция Тест()\n    {stmt}\nКонецФункции\n"
            ),
            Owner::Unheld
            | Owner::UnheldSynthetic
            | Owner::ObjectAttribute
            | Owner::FormAttribute
            | Owner::CommonModule => {
                format!("Функция Тест()\n    {stmt}\nКонецФункции\n")
            }
        }
    }
}

/// A ready-to-query module, plus its own text so callers can locate offsets.
pub(super) struct Scenario {
    /// Kept alive: dropping it deletes the configuration off disk.
    _temp: Option<TempDir>,
    pub(super) analysis: Analysis,
    pub(super) file_id: FileId,
    pub(super) source: String,
}

impl Scenario {
    /// Byte offset of `needle` in the module text.
    pub(super) fn offset_of(&self, needle: &str) -> u32 {
        self.source
            .find(needle)
            .unwrap_or_else(|| panic!("scenario source must contain {needle:?}:\n{}", self.source))
            as u32
    }

    pub(super) fn whole_range(&self) -> syntax::TextRange {
        syntax::TextRange::new(0.into(), syntax::TextSize::from(self.source.len() as u32))
    }
}

pub(super) fn scenario(owner: Owner, stmt: &str) -> Scenario {
    let source = owner.wrap(stmt);
    match owner {
        Owner::Unheld
        | Owner::ImplicitAssignment
        | Owner::Parameter
        | Owner::ModuleVariable
        | Owner::ModuleMethod => {
            let (analysis, file_id) =
                build(&format!("//- /test.bsl\n{source}"), "/test.bsl", &designer_fixture_path());
            Scenario { _temp: None, analysis, file_id, source }
        }
        Owner::UnheldSynthetic | Owner::ObjectAttribute | Owner::FormAttribute => {
            let temp = tempfile::tempdir().expect("create synthetic config tempdir");
            write_member_collision_config(temp.path());
            let module_path = match owner {
                Owner::ObjectAttribute => "Catalogs/Справочник1/Ext/ObjectModule.bsl",
                Owner::FormAttribute => {
                    "Catalogs/Справочник1/Forms/ФормаЭлемента/Ext/Form/Module.bsl"
                }
                _ => "CommonModules/Тестовый/Ext/Module.bsl",
            };
            let full = temp.path().join(module_path);
            let (analysis, file_id) = build(
                &format!("//- {}\n{source}", full.display()),
                &full.to_string_lossy(),
                temp.path(),
            );
            Scenario { _temp: Some(temp), analysis, file_id, source }
        }
        Owner::CommonModule => {
            let temp = tempfile::tempdir().expect("create synthetic config tempdir");
            write_common_module_collision_config(temp.path());
            let holder = temp.path().join("CommonModules/Справочники/Ext/Module.bsl");
            let full = temp.path().join("CommonModules/Тестовый/Ext/Module.bsl");
            let fixture = format!(
                "//- {}\nФункция МойМетод() Экспорт\n    Возврат 1;\nКонецФункции\n\n//- {}\n{source}",
                holder.display(),
                full.display()
            );
            let (analysis, file_id) = build(&fixture, &full.to_string_lossy(), temp.path());
            Scenario { _temp: Some(temp), analysis, file_id, source }
        }
    }
}

fn build(fixture_text: &str, cursor_file: &str, config_path: &Path) -> (Analysis, FileId) {
    let fixture = Fixture::parse(fixture_text);
    let mut db = RootDatabaseImpl::new();
    let mut file_set = FileSet::default();
    for (file_id, file) in &fixture.files {
        file_set.insert(*file_id, file.path.clone());
    }
    db.set_source_root(SourceRootId(0), SourceRoot::new_local(file_set));
    for (file_id, file) in &fixture.files {
        db.set_file_source_root(*file_id, SourceRootId(0));
        db.set_file_text(*file_id, &file.content);
    }
    db.set_all_config_paths(vec![(None, config_path.to_path_buf())], None);
    let file_id = fixture
        .files
        .iter()
        .find(|(_, f)| f.path.as_path().to_string_lossy() == cursor_file)
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("fixture must contain {cursor_file}"));
    (Analysis::from_database(db), file_id)
}

fn designer_fixture_path() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../bsl-metadata/fixtures/designer"))
}

/// A configuration whose own members are named after the `Справочники` manager
/// collection. `Справочник1` is declared as well: without a resolvable catalog
/// the chain would fall silent for lack of a target rather than for shadowing,
/// and the positive control could not tell the two apart.
fn write_member_collision_config(root: &Path) {
    std::fs::create_dir_all(root.join("Catalogs")).expect("create Catalogs directory");
    std::fs::create_dir_all(root.join("Catalogs/Справочник1/Forms/ФормаЭлемента/Ext"))
        .expect("create form directory");
    std::fs::write(
        root.join("Configuration.xml"),
        configuration_xml("ManagerRootCollisionConfig", &["Тестовый"]),
    )
    .expect("write synthetic Configuration.xml");
    write_common_module(root, "Тестовый");
    std::fs::write(
        root.join("Catalogs/Справочник1.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" xmlns:v8="http://v8.1c.ru/8.1/data/core" version="2.10">
    <Catalog uuid="22222222-2222-2222-2222-222222222222">
        <Properties>
            <Name>Справочник1</Name>
            <CodeLength>9</CodeLength>
            <DescriptionLength>25</DescriptionLength>
        </Properties>
        <ChildObjects>
            <Attribute uuid="33333333-3333-3333-3333-333333333333">
                <Properties>
                    <Name>Справочники</Name>
                    <Type>
                        <v8:Type>xs:string</v8:Type>
                        <v8:StringQualifiers>
                            <v8:Length>20</v8:Length>
                        </v8:StringQualifiers>
                    </Type>
                </Properties>
            </Attribute>
            <Form>ФормаЭлемента</Form>
        </ChildObjects>
    </Catalog>
</MetaDataObject>"#,
    )
    .expect("write synthetic catalog XML");
    std::fs::write(
        root.join("Catalogs/Справочник1/Forms/ФормаЭлемента/Ext/Form.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Form xmlns="http://v8.1c.ru/8.3/xcf/logform" version="2.10">
    <Properties>
        <Name>ФормаЭлемента</Name>
    </Properties>
    <Attributes>
        <Attribute name="Справочники" id="1">
            <Type/>
        </Attribute>
    </Attributes>
</Form>"#,
    )
    .expect("write synthetic Form.xml");
}

/// The same collision carried by a workspace common module. It shadows
/// workspace-wide, so it cannot share a configuration with a positive control.
fn write_common_module_collision_config(root: &Path) {
    std::fs::create_dir_all(root.join("Catalogs")).expect("create Catalogs directory");
    std::fs::write(
        root.join("Configuration.xml"),
        configuration_xml("ManagerRootModuleCollisionConfig", &["Справочники", "Тестовый"]),
    )
    .expect("write synthetic Configuration.xml");
    write_common_module(root, "Справочники");
    write_common_module(root, "Тестовый");
    std::fs::write(
        root.join("Catalogs/Справочник1.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" xmlns:v8="http://v8.1c.ru/8.1/data/core" version="2.10">
    <Catalog uuid="22222222-2222-2222-2222-222222222222">
        <Properties>
            <Name>Справочник1</Name>
            <CodeLength>9</CodeLength>
            <DescriptionLength>25</DescriptionLength>
        </Properties>
    </Catalog>
</MetaDataObject>"#,
    )
    .expect("write synthetic catalog XML");
}

fn configuration_xml(name: &str, common_modules: &[&str]) -> String {
    let modules: String = common_modules
        .iter()
        .map(|m| format!("            <CommonModule>{m}</CommonModule>\n"))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" version="2.10">
    <Configuration uuid="11111111-1111-1111-1111-111111111111">
        <Properties>
            <Name>{name}</Name>
        </Properties>
        <ChildObjects>
{modules}            <Catalog>Справочник1</Catalog>
        </ChildObjects>
    </Configuration>
</MetaDataObject>"#
    )
}

/// The configuration loader discovers a common module from its on-disk
/// presence, so the module directory has to exist even when the body itself is
/// supplied virtually through the fixture.
fn write_common_module(root: &Path, name: &str) {
    std::fs::create_dir_all(root.join(format!("CommonModules/{name}/Ext")))
        .expect("create common module directory");
    std::fs::write(root.join(format!("CommonModules/{name}/Ext/Module.bsl")), "")
        .expect("write common module body placeholder");
    std::fs::write(root.join(format!("CommonModules/{name}.xml")), common_module_xml(name))
        .expect("write common module XML");
}

fn common_module_xml(name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses" version="2.10">
    <CommonModule uuid="44444444-4444-4444-4444-4444444444{:02x}">
        <Properties>
            <Name>{name}</Name>
            <Global>false</Global>
            <Server>true</Server>
            <ClientManagedApplication>false</ClientManagedApplication>
            <ClientOrdinaryApplication>false</ClientOrdinaryApplication>
            <ExternalConnection>false</ExternalConnection>
            <ServerCall>false</ServerCall>
            <Privileged>false</Privileged>
            <ReturnValuesReuse>DontUse</ReturnValuesReuse>
        </Properties>
    </CommonModule>
</MetaDataObject>"#,
        name.len()
    )
}
