# Native extension common-attribute fixtures

These XML files are copied byte-for-byte from the native G1 exports for issue #379 in `.artifacts/issue379-native/g1-20261010T233319Z-f946153f/native-exports/`:

- `canonical-base-v1/base` → `base`
- `canonical-no-common-v1/no-common` → `extensions/no-common`
- `canonical-explicit-use-v1/explicit-use` → `extensions/explicit-use`
- `canonical-explicit-dont-use-v1/explicit-dont-use` → `extensions/explicit-dont-use`

The exports were produced by 1C:Enterprise 8.3.27.1989 with compatibility mode `Version8_3_27`. `ConfigDumpInfo.xml` and BSL module files are intentionally excluded; no export content is synthesized. `SHA256SUMS` hashes every checked-in XML file relative to this directory. The source export hashes are recorded in the run's `g1-hashes.json`.
