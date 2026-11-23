## How code is stored in Kodehub (read carefully — your output is compiled exactly as written)

Code lives as **symbols** (stable identity, `display_name`) with immutable **versions**
(`signature` + `body` + `attributes`). Files are generated from the graph; never write file
headers, `mod`, `package`, or `import`/`use` lines inside bodies.

- **signature is name-free.** The renderer writes `pub fn <display_name><signature> { <body> }`.
  - Rust function: `(store: &UserStore, username: &str) -> AuthResult`
  - Rust method (display name `UserStore::find_user`): `(&self, username: &str) -> Option<User>`
  - Python function: `(username: str, password: str) -> dict`
  - Rust struct / enum / Python class: signature is `""` (or generics / bases like `(Base)`).
  - Tests: signature `""`.
- **body excludes the enclosing braces / `def` line** and is written unindented.
  - Rust struct body = the field list, e.g. `pub username: String,\npub locked: bool,`
  - Rust enum body = the variant list, e.g. `Authenticated { username: String },\nInvalidCredentials,`
  - Rust function body = the statements inside `{ }` (final expression allowed).
  - Rust test body = statements inside the `#[test] fn` (assert macros etc.). Tests live in a
    `mod kodehub_tests { use super::*; }` so every public item is in scope.
  - Python test body = statements inside `def test_x():` (plain `assert`).
- **Other symbols are referenced by their display name** (methods by method syntax
  `store.find_user(..)` or `UserStore::new()`). Every item of the same language is in one module,
  so no imports are needed between symbols.
- **attributes** (JSON object, often `{}`):
  - Rust: `uses` (list of `use` paths, e.g. `"std::collections::HashMap"`), `crates`
    (map name → version string or `{version, features}`), `derives` (e.g. `["Debug","Clone"]`;
    serde Serialize/Deserialize are added automatically), `item_attrs` (raw attribute lines such
    as `#[serde(rename_all = "snake_case")]`).
  - Python: `imports` (full import lines), `packages` (pip requirement strings), `decorators`.
  - When you change an existing symbol, **repeat its existing attributes** unless you mean to
    change them.
- New struct fields that old data may lack should have `#[serde(default)]`.
- Keep functions small and idiomatic. No `unwrap()` on user input. Do not print.
